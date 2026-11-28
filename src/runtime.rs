use std::{
    env, fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, WaitIdStatus, kill_process_group, waitid,
};
use tempfile::{Builder, TempDir};
use tokio::{
    io::{AsyncReadExt, BufReader},
    process::{ChildStdout, Command},
    sync::watch,
    task::JoinHandle,
    time,
};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod capture_link;
use capture_link::CaptureLink;

const DISPLAY_NAME: &str = "mcp-wayland";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(8);
const FRAME_WAIT: Duration = Duration::from_millis(350);
const MAX_PNG_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct Size {
    pub width: u32,
    pub height: u32,
}

pub(crate) struct Program {
    pub path: PathBuf,
    pub args: Vec<std::ffi::OsString>,
}

pub(crate) struct Runtime {
    mutter: Process,
    wireplumber: Process,
    pipewire: Process,
    bus: Process,
    env: PrivateEnv,
    _dir: TempDir,
}

impl Runtime {
    pub async fn start(size: Size, stopping: &CancellationToken) -> Result<Self> {
        let dir = Builder::new()
            .prefix("gwm-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .context("create private runtime directory")?;
        let env = PrivateEnv::new(dir.path())?;
        let bus_config = dir.path().join("bus.conf");
        fs::write(
            &bus_config,
            format!(
                r#"<busconfig>
  <type>session</type>
  <listen>{}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
    <allow own="*"/>
  </policy>
</busconfig>"#,
                env.bus_address
            ),
        )?;

        let mut services = Vec::with_capacity(4);
        let startup = async {
            env.start_service(&mut services, "dbus-daemon", |command| {
                command
                    .arg("--nofork")
                    .arg(format!("--config-file={}", bus_config.display()));
            })?
            .wait_for_socket(&dir.path().join("bus"))
            .await?;

            env.start_service(&mut services, "pipewire", |command| {
                command.args(["-c", "private.conf"]);
            })?
            .wait_for_socket(&dir.path().join("pipewire-0"))
            .await?;

            env.start_service(&mut services, "wireplumber", |command| {
                command.args(["--profile", "policy"]);
            })?;

            env.start_service(&mut services, "mutter", |command| {
                command
                    .args([
                        "--headless",
                        "--wayland",
                        "--no-x11",
                        "--wayland-display",
                        DISPLAY_NAME,
                    ])
                    .arg("--virtual-monitor")
                    .arg(format!("{}x{}", size.width, size.height));
            })?
            .wait_for_socket(&dir.path().join(DISPLAY_NAME))
            .await?;
            services.iter().try_for_each(Process::check)
        };
        let result = stopping
            .run_until_cancelled(startup)
            .await
            .unwrap_or_else(|| Err(anyhow!("desktop startup cancelled")));
        if let Err(error) = result {
            // Service ownership stays outside the cancellable future. Reap any
            // children that started before deleting the runtime directory.
            for process in services.iter_mut().rev() {
                process.stop().await;
            }
            return Err(error);
        }
        let [bus, pipewire, wireplumber, mutter] =
            services.try_into().ok().expect("four started services");
        let runtime = Self {
            mutter,
            wireplumber,
            pipewire,
            bus,
            env,
            _dir: dir,
        };
        Ok(runtime)
    }

    pub fn bus_path(&self) -> PathBuf {
        self.env.root.join("bus")
    }

    pub fn mutter_pid(&self) -> u32 {
        self.mutter.pid.as_raw_nonzero().get() as u32
    }

    pub fn check(&self) -> Result<()> {
        self.bus.check()?;
        self.pipewire.check()?;
        self.wireplumber.check()?;
        self.mutter.check()
    }

    pub fn launch(&self, program: &Program) -> Result<Process> {
        let mut command = self.env.command(&program.path);
        command
            .args(&program.args)
            .current_dir(&self.env.home)
            .stderr(Stdio::null());
        Process::spawn("configured application", &mut command)
    }

    pub async fn start_capture(
        &self,
        node: u32,
        size: Size,
        stopping: &CancellationToken,
    ) -> Result<Capture> {
        self.check()?;
        let mut command = self.env.command(Path::new("/usr/bin/gst-launch-1.0"));
        command.args([
            "-q",
            "pipewiresrc",
            &format!("path={node}"),
            "on-disconnect=error",
            "!",
            "queue",
            "max-size-buffers=2",
            "leaky=downstream",
            "!",
            "videorate",
            "drop-only=true",
            "max-rate=8",
            "!",
            "videoconvert",
            "!",
            "pngenc",
            "!",
            "fdsink",
            "fd=1",
            "sync=false",
        ]);
        command
            .stdout(Stdio::piped())
            .stderr(self.env.service_log("capture")?);
        let mut process = Process::spawn("GStreamer capture", &mut command)?;
        let Some(output) = process.child.stdout.take() else {
            process.stop().await;
            bail!("open private capture pipe");
        };
        let (tx, mut frames) = watch::channel(None);
        let mut reader = tokio::spawn(read_pngs(output, size, tx));
        let open = async {
            if frames.changed().await.is_err() {
                let reason = match (&mut reader).await {
                    Ok(Err(error)) => format!("{error:#}"),
                    _ => "frame reader stopped".to_owned(),
                };
                bail!(
                    "unable to open private capture stream (pipewiresrc must support on-disconnect=error): {reason}"
                );
            }
            CaptureLink::bind(&self.env, node, process.pid.as_raw_nonzero().get() as u32).await
        };
        let error = match stopping
            .run_until_cancelled(time::timeout(CAPTURE_TIMEOUT, open))
            .await
        {
            Some(Ok(Ok(link))) => {
                return Ok(Capture {
                    process,
                    reader,
                    frames,
                    link,
                    frame_id: None,
                });
            }
            Some(Ok(Err(error))) => error,
            Some(Err(_)) => {
                anyhow!("private capture timed out; check PipeWire and GStreamer plugins")
            }
            None => anyhow!("desktop startup cancelled"),
        };
        reader.abort();
        process.stop().await;
        Err(error)
    }

    pub async fn stop(&mut self) {
        self.mutter.stop().await;
        self.wireplumber.stop().await;
        self.pipewire.stop().await;
        self.bus.stop().await;
    }
}

struct PrivateEnv {
    root: PathBuf,
    home: PathBuf,
    bus_address: String,
}

impl PrivateEnv {
    fn new(root: &Path) -> Result<Self> {
        for name in ["home", "config", "cache", "data", "state"] {
            fs::create_dir(root.join(name))?;
        }
        // Take control of the core config so host drop-ins cannot add hardware discovery.
        let pipewire_config = root.join("config/pipewire");
        fs::create_dir(&pipewire_config)?;
        fs::write(
            pipewire_config.join("private.conf"),
            include_str!("private-pipewire.conf"),
        )?;
        Ok(Self {
            root: root.to_path_buf(),
            home: root.join("home"),
            bus_address: format!("unix:path={}/bus", root.display()),
        })
    }

    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command.stdout(Stdio::null());
        // In particular, inherited WAYLAND_SOCKET, DISPLAY, bus addresses and device
        // configuration must never identify the interactive desktop.
        command.env_clear();
        for key in [
            "LANG",
            "LANGUAGE",
            "LC_ALL",
            "LC_CTYPE",
            "LC_MESSAGES",
            "TZ",
        ] {
            if let Some(value) = env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &self.home)
            .env("XDG_RUNTIME_DIR", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_SESSION_TYPE", "wayland")
            .env("XDG_CURRENT_DESKTOP", "GNOME")
            .env("WAYLAND_DISPLAY", DISPLAY_NAME)
            .env("DBUS_SESSION_BUS_ADDRESS", &self.bus_address)
            .env(
                "DBUS_SYSTEM_BUS_ADDRESS",
                format!("unix:path={}/no-system-bus", self.root.display()),
            )
            .env("PIPEWIRE_RUNTIME_DIR", &self.root)
            .env("PIPEWIRE_REMOTE", "pipewire-0")
            .env(
                "PULSE_SERVER",
                format!("unix:{}/no-pulse-server", self.root.display()),
            )
            .env("GDK_BACKEND", "wayland")
            .env("QT_QPA_PLATFORM", "wayland")
            .env("GSETTINGS_BACKEND", "memory")
            .env("GIO_USE_VFS", "local");
        command
    }

    fn start_service<'a>(
        &self,
        services: &'a mut Vec<Process>,
        name: &'static str,
        configure: impl FnOnce(&mut Command),
    ) -> Result<&'a Process> {
        // Fixed system paths cannot accidentally resolve a similarly named project command.
        let mut command = self.command(&Path::new("/usr/bin").join(name));
        command.stderr(self.service_log(name)?);
        configure(&mut command);
        services.push(Process::spawn(name, &mut command)?);
        Ok(services.last().expect("the service was pushed"))
    }

    fn service_log(&self, name: &str) -> Result<Stdio> {
        Ok(Stdio::from(fs::File::create(
            self.root.join(format!("{name}.log")),
        )?))
    }
}

pub(crate) struct Process {
    name: &'static str,
    child: tokio::process::Child,
    pid: Pid,
}

impl Process {
    fn spawn(name: &'static str, command: &mut Command) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .process_group(0)
            .spawn()
            .with_context(|| format!("start {name}"))?;
        let Some(pid) = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(Pid::from_raw)
        else {
            let _ = child.start_kill();
            bail!("could not identify {name} process");
        };
        Ok(Self { name, child, pid })
    }

    pub fn running(&self) -> Result<bool> {
        Ok(self.exit_state()?.is_none())
    }

    fn check(&self) -> Result<()> {
        if let Some(status) = self.exit_state()? {
            bail!("{} exited unexpectedly: {status:?}", self.name);
        }
        Ok(())
    }

    pub(crate) fn exit_state(&self) -> Result<Option<WaitIdStatus>> {
        // Keep the process-group leader waitable until we have stopped its group.
        Ok(waitid(
            WaitId::Pid(self.pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )?)
    }

    async fn wait_for_socket(&self, path: &Path) -> Result<()> {
        use std::os::unix::fs::FileTypeExt;
        time::timeout(STARTUP_TIMEOUT, async {
            loop {
                self.check()?;
                match fs::symlink_metadata(path) {
                    Ok(metadata) if metadata.file_type().is_socket() => return Ok(()),
                    Ok(_) => bail!("expected private Unix socket at {}", path.display()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                }
                time::sleep(Duration::from_millis(40)).await;
            }
        })
        .await
        .with_context(|| format!("timed out waiting for {}", path.display()))?
    }

    pub async fn stop(&mut self) {
        self.signal_group();
        let _ = self.child.wait().await;
    }

    fn signal_group(&mut self) {
        // Signal before reaping. After wait, the old PID can be reused.
        if self.child.id().is_some() {
            let _ = kill_process_group(self.pid, Signal::KILL);
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.signal_group();
    }
}

pub(crate) struct Capture {
    process: Process,
    reader: JoinHandle<Result<()>>,
    frames: watch::Receiver<Option<Arc<[u8]>>>,
    link: CaptureLink,
    /// The pointer token belongs to the stream that supplied its pixels.
    pub frame_id: Option<Uuid>,
}

#[derive(PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CaptureHealth {
    Ready,
    Paused,
    RecoveryNeeded,
}

impl CaptureHealth {
    pub fn require_ready(self) -> Result<()> {
        match self {
            Self::Ready => Ok(()),
            Self::Paused => {
                bail!("private capture is paused; take a new screenshot after it resumes")
            }
            Self::RecoveryNeeded => bail!("private capture was interrupted; take a new screenshot"),
        }
    }
}

impl Capture {
    pub fn link_interrupted(&self) -> &CancellationToken {
        &self.link.interrupted
    }

    pub fn is_closed(&self) -> Result<bool> {
        Ok(self.process.child.id().is_none()
            || self.frames.has_changed().is_err()
            || !self.process.running()?)
    }

    pub async fn health(&self, runtime: &Runtime) -> Result<CaptureHealth> {
        if self.is_closed()? || !self.link.is_active(&runtime.env).await? {
            return Ok(CaptureHealth::RecoveryNeeded);
        }
        self.observed_health()
    }

    /// Check the monitor and reader between drag steps without spawning a graph query.
    pub fn observed_health(&self) -> Result<CaptureHealth> {
        if self.link.interrupted.is_cancelled() || self.is_closed()? {
            return Ok(CaptureHealth::RecoveryNeeded);
        }
        // Leave stop/continue events waitable so an interruption remains
        // visible until this capture is replaced, even if it already resumed.
        let event = waitid(
            WaitId::Pid(self.process.pid),
            WaitIdOptions::EXITED
                | WaitIdOptions::STOPPED
                | WaitIdOptions::CONTINUED
                | WaitIdOptions::NOHANG
                | WaitIdOptions::NOWAIT,
        )?;
        Ok(match event {
            Some(event) if event.stopped() || event.trapped() => CaptureHealth::Paused,
            Some(_) => CaptureHealth::RecoveryNeeded,
            None => CaptureHealth::Ready,
        })
    }

    pub async fn require_uninterrupted(&self, runtime: &Runtime) -> Result<()> {
        self.health(runtime).await?.require_ready()
    }

    pub async fn wait_for_frame(&mut self) {
        self.frames.borrow_and_update();
        // A static monitor may produce no new buffer. The caller checks the link
        // before publishing a frame, including after EOF or a timeout here.
        let _ = time::timeout(FRAME_WAIT, self.frames.changed()).await;
    }

    pub fn latest_frame(&self) -> Result<Arc<[u8]>> {
        self.frames
            .borrow()
            .clone()
            .context("private capture has no frame")
    }

    pub async fn stop(&mut self) {
        self.reader.abort();
        self.link.stop().await;
        self.process.stop().await;
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

async fn read_pngs(
    output: ChildStdout,
    size: Size,
    frames: watch::Sender<Option<Arc<[u8]>>>,
) -> Result<()> {
    let mut source = BufReader::new(output);
    let mut start = [0; 8];
    loop {
        source
            .read_exact(&mut start)
            .await
            .context("read PNG signature from GStreamer")?;
        ensure!(
            &start == b"\x89PNG\r\n\x1a\n",
            "GStreamer returned a non-PNG capture"
        );
        let mut png = start.to_vec();
        loop {
            let mut chunk = [0; 8];
            source
                .read_exact(&mut chunk)
                .await
                .context("read PNG chunk header")?;
            let length = u32::from_be_bytes(chunk[..4].try_into()?) as usize;
            ensure!(
                length <= MAX_PNG_BYTES.saturating_sub(png.len() + 12),
                "capture exceeds 10 MiB"
            );
            let is_end = &chunk[4..] == b"IEND";
            ensure!(!is_end || length == 0, "invalid PNG end chunk");
            png.extend_from_slice(&chunk);
            let offset = png.len();
            png.resize(offset + length + 4, 0);
            source
                .read_exact(&mut png[offset..])
                .await
                .context("read PNG chunk body")?;
            if is_end {
                break;
            }
        }
        ensure!(
            png_size(&png) == Some((size.width, size.height)),
            "screenshot dimensions do not match the private monitor"
        );
        frames.send_replace(Some(Arc::from(png)));
    }
}

fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(..16)? != b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR" {
        return None;
    }
    Some((
        u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?),
        u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?),
    ))
}
