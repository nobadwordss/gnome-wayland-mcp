use std::{collections::HashMap, future::Future, os::fd::OwnedFd, path::Path, time::Duration};

use anyhow::{Context, Result, anyhow, ensure};
use futures_util::StreamExt;
use tokio::{io::AsyncWriteExt, net::unix::pipe::Sender, sync::oneshot, task::JoinHandle, time};
use tokio_util::sync::CancellationToken;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{
    Connection, Proxy,
    fdo::DBusProxy,
    names::{BusName, OwnedUniqueName},
};

use crate::runtime::Size;

mod guarded_input;

const REMOTE_DESKTOP: &str = "org.gnome.Mutter.RemoteDesktop";
const REMOTE_DESKTOP_PATH: &str = "/org/gnome/Mutter/RemoteDesktop";
const REMOTE_SESSION: &str = "org.gnome.Mutter.RemoteDesktop.Session";
const SCREEN_CAST: &str = "org.gnome.Mutter.ScreenCast";
const SCREEN_CAST_PATH: &str = "/org/gnome/Mutter/ScreenCast";
const CAST_SESSION: &str = "org.gnome.Mutter.ScreenCast.Session";
const CAST_STREAM: &str = "org.gnome.Mutter.ScreenCast.Stream";
const CLIPBOARD: ClipboardApi = ClipboardApi {
    interface: "org.gnome.Mutter.Clipboard",
    enable: "Enable",
    disable: "Disable",
};
const LEGACY_CLIPBOARD: ClipboardApi = ClipboardApi {
    interface: REMOTE_SESSION,
    enable: "EnableClipboard",
    disable: "DisableClipboard",
};
const DBUS_TIMEOUT: Duration = Duration::from_secs(2);
const STREAM_TIMEOUT: Duration = Duration::from_secs(7);
const PASTE_TIMEOUT: Duration = Duration::from_secs(4);
const TEXT_MIME_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

pub(crate) struct Mutter {
    connection: Connection,
    owner: OwnedUniqueName,
    remote_path: OwnedObjectPath,
    stream_path: OwnedObjectPath,
    pub node: u32,
    clipboard_api: ClipboardApi,
    clipboard_enabled: bool,
    clipboard_task: Option<ClipboardTask>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ClipboardApi {
    interface: &'static str,
    enable: &'static str,
    disable: &'static str,
}

struct ClipboardTask {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Mutter {
    pub async fn connect(bus_path: &Path, pid: u32, size: Size) -> Result<Self> {
        let connection = time::timeout(DBUS_TIMEOUT, guarded_input::connect(bus_path))
            .await
            .context("connect to private D-Bus")??;
        let owner = verify_owner(&connection, pid).await?;
        let remote = Proxy::new(
            &connection,
            owner.as_str(),
            REMOTE_DESKTOP_PATH,
            REMOTE_DESKTOP,
        )
        .await?;
        let remote_path: OwnedObjectPath = remote
            .call("CreateSession", &())
            .await
            .context("create private input session")?;
        let remote_session = Proxy::new(
            &connection,
            owner.as_str(),
            remote_path.as_str(),
            REMOTE_SESSION,
        )
        .await?;
        let session_id: String = remote_session.get_property("SessionId").await?;
        let cast = Proxy::new(&connection, owner.as_str(), SCREEN_CAST_PATH, SCREEN_CAST).await?;
        let cast_options = HashMap::from([("remote-desktop-session-id", Value::from(session_id))]);
        let cast_path: OwnedObjectPath = cast
            .call("CreateSession", &(cast_options,))
            .await
            .context("create private capture session")?;
        let capture_session = Proxy::new(
            &connection,
            owner.as_str(),
            cast_path.as_str(),
            CAST_SESSION,
        )
        .await?;
        let monitor_options = HashMap::from([("cursor-mode", Value::from(1_u32))]);
        let stream_path: OwnedObjectPath = capture_session
            .call("RecordMonitor", &("", monitor_options))
            .await
            .context("record private monitor")?;
        let stream = Proxy::new(
            &connection,
            owner.as_str(),
            stream_path.as_str(),
            CAST_STREAM,
        )
        .await?;
        let parameters: HashMap<String, OwnedValue> = stream.get_property("Parameters").await?;
        let position = pair(&parameters, "position")?;
        let extent = pair(&parameters, "size")?;
        ensure!(
            position == (0, 0) && extent == (size.width as i32, size.height as i32),
            "Mutter stream does not match the private monitor geometry"
        );

        let mut added = stream.receive_signal("PipeWireStreamAdded").await?;
        remote_session
            .call::<_, _, ()>("Start", &())
            .await
            .context("start private input and capture")?;
        let message = time::timeout(STREAM_TIMEOUT, added.next())
            .await
            .context("waiting for private PipeWire stream")?
            .context("Mutter closed the capture channel")?;
        let node: u32 = message.body().deserialize()?;
        drop(added);
        drop(stream);
        drop(capture_session);
        drop(cast);
        drop(remote_session);
        drop(remote);
        Ok(Self {
            connection,
            owner,
            remote_path,
            stream_path,
            node,
            clipboard_api: CLIPBOARD,
            clipboard_enabled: false,
            clipboard_task: None,
        })
    }

    async fn remote(&self) -> Result<Proxy<'_>> {
        Ok(Proxy::new(
            &self.connection,
            self.owner.as_str(),
            self.remote_path.as_str(),
            REMOTE_SESSION,
        )
        .await?)
    }

    pub async fn move_pointer(
        &self,
        x: u32,
        y: u32,
        interrupted: Option<&CancellationToken>,
    ) -> Result<()> {
        let remote = self.remote().await?;
        let args = (
            self.stream_path.as_str(),
            f64::from(x) + 0.5,
            f64::from(y) + 0.5,
        );
        let motion = remote.call("NotifyPointerMotionAbsolute", &args);
        match interrupted {
            Some(token) => guarded_input::send(token.cancelled(), motion).await.1,
            None => motion.await,
        }
        .context("move private pointer")
    }

    pub async fn with_button_down(
        &self,
        button: i32,
        interrupted: impl Future<Output = ()>,
        any_press_started: &mut bool,
        while_held: impl Future<Output = Result<()>>,
    ) -> Result<()> {
        let remote = self.remote().await?;
        let (started, press) = guarded_input::send(
            interrupted,
            remote.call("NotifyPointerButton", &(button, true)),
        )
        .await;
        if !started && press.is_err() {
            return press.context("send private button press");
        }
        *any_press_started = true;
        let action = match press {
            Ok(()) => while_held.await,
            Err(error) => Err(error.into()),
        };
        // Always attempt release after a possible press, including on motion failure.
        let release = remote
            .call::<_, _, ()>("NotifyPointerButton", &(button, false))
            .await
            .context("release private pointer button");
        release.and(action)
    }

    pub async fn scroll(&self, horizontal: i32, vertical: i32) -> Result<()> {
        let remote = self.remote().await?;
        for (axis, steps) in [(0_u32, vertical), (1_u32, horizontal)] {
            if steps != 0 {
                remote
                    .call::<_, _, ()>("NotifyPointerAxisDiscrete", &(axis, steps))
                    .await
                    .context("send private scroll")?;
            }
        }
        Ok(())
    }

    pub async fn key(&self, keysym: u32, modifiers: &[u32]) -> Result<()> {
        let remote = self.remote().await?;
        send_key(&remote, keysym, modifiers)
            .await
            .context("send private key combination")
    }

    pub async fn prepare_input(&self, size: Size) -> Result<()> {
        // The first notifications create virtual devices in Mutter. Warm them
        // here, before the configured application starts receiving input.
        self.move_pointer(size.width / 2, size.height / 2, None)
            .await?;
        self.key(0xffe1, &[]).await // Shift_L
    }

    pub async fn paste(&mut self, text: String, modifiers: &[u32]) -> Result<()> {
        self.paste_with_timeout(text, modifiers, PASTE_TIMEOUT)
            .await
    }

    async fn clipboard_proxy(&self) -> zbus::Result<Proxy<'static>> {
        Proxy::new_owned(
            self.connection.clone(),
            self.owner.clone(),
            self.remote_path.clone(),
            self.clipboard_api.interface,
        )
        .await
    }

    async fn paste_with_timeout(
        &mut self,
        text: String,
        modifiers: &[u32],
        timeout: Duration,
    ) -> Result<()> {
        self.stop_clipboard_task().await;
        let mut clipboard = self.clipboard_proxy().await?;
        let paste = async {
            let options = (HashMap::<&str, Value<'_>>::new(),);
            if self.clipboard_enabled {
                // Retire the old source before subscribing. A late signal emitted
                // for it during replacement has no source ID to match against.
                clipboard
                    .call::<_, _, ()>("SetSelection", &options)
                    .await
                    .context("clear the previous private clipboard source")?;
            } else {
                let enabled = clipboard
                    .call::<_, _, ()>(self.clipboard_api.enable, &options)
                    .await;
                match enabled {
                    Err(zbus::Error::MethodError(name, _, _))
                        if self.clipboard_api == CLIPBOARD
                            && matches!(
                                name.as_str(),
                                "org.freedesktop.DBus.Error.UnknownInterface"
                                    | "org.freedesktop.DBus.Error.UnknownMethod"
                            ) =>
                    {
                        // Older Mutter exposes the entire clipboard API on the input session.
                        self.clipboard_api = LEGACY_CLIPBOARD;
                        clipboard = self.clipboard_proxy().await?;
                        clipboard
                            .call::<_, _, ()>(self.clipboard_api.enable, &options)
                            .await
                            .context("enable private clipboard on the remote desktop session")?;
                    }
                    result => result.context("enable private clipboard")?,
                }
                self.clipboard_enabled = true;
            }

            let mut transfers = clipboard.receive_signal("SelectionTransfer").await?;
            let mime_types = HashMap::from([("mime-types", Value::from(TEXT_MIME_TYPES.to_vec()))]);
            clipboard
                .call::<_, _, ()>("SetSelection", &(mime_types,))
                .await
                .context("set private clipboard text")?;

            let clipboard = clipboard.clone();
            let (stop, mut stopping) = oneshot::channel();
            let (started, first_request) = oneshot::channel();
            let (finished, first_completion) = oneshot::channel();
            let task = tokio::spawn(async move {
                let mut first = Some((started, finished));
                while let Some(message) = tokio::select! {
                    biased;
                    _ = &mut stopping => None,
                    message = transfers.next() => message,
                } {
                    let completion = first.take().map(|(started, finished)| {
                        let _ = started.send(());
                        finished
                    });
                    let transfer = transfer_text(&clipboard, &message, text.as_bytes(), timeout).await;
                    if let Some(completion) = completion {
                        let _ = completion.send(transfer);
                    }
                }
            });
            self.clipboard_task = Some(ClipboardTask { stop, task });

            // Resolve v in the active layout and hold Shift explicitly for the shifted chord.
            self.key(u32::from('v'), modifiers).await?;
            time::timeout(timeout, first_request)
                .await
                .context("no private clipboard transfer arrived in time. Check the private desktop before retrying")?
                .context("Mutter closed the clipboard channel")?;
            // The write has its own deadline. Wait for its completion notice
            // without cancelling it. A cache request cannot confirm insertion.
            first_completion.await.context("Mutter closed the clipboard channel")?
        }
        .await;
        if paste.is_err() {
            self.stop_clipboard_task().await;
            if self.clipboard_enabled {
                let disabled = clipboard
                    .call::<_, _, ()>(self.clipboard_api.disable, &())
                    .await
                    .is_ok();
                self.clipboard_enabled = !disabled;
            }
        }
        paste
    }

    async fn stop_clipboard_task(&mut self) {
        if let Some(ClipboardTask { stop, task }) = self.clipboard_task.take() {
            // An in-flight write owns an FD and must finish or time out and
            // acknowledge failure before we retire its clipboard source.
            let _ = stop.send(());
            let _ = task.await;
        }
    }

    pub async fn stop(&mut self) {
        self.stop_clipboard_task().await;
        if let Ok(remote) = self.remote().await {
            let _ = remote.call::<_, _, ()>("Stop", &()).await;
        }
    }
}

impl Drop for Mutter {
    fn drop(&mut self) {
        if let Some(ClipboardTask { task, .. }) = self.clipboard_task.take() {
            task.abort();
        }
    }
}

async fn transfer_text(
    clipboard: &Proxy<'_>,
    message: &zbus::Message,
    text: &[u8],
    timeout: Duration,
) -> Result<()> {
    let (mime, serial): (String, u32) = message.body().deserialize()?;
    ensure!(
        TEXT_MIME_TYPES.contains(&mime.as_str()),
        "application requested unsupported clipboard format"
    );
    let fd: zbus::zvariant::OwnedFd = clipboard
        .call("SelectionWrite", &(serial,))
        .await
        .context("open private clipboard transfer")?;
    let transfer = time::timeout(timeout, write_fd(fd.into(), text))
        .await
        .context("clipboard transfer timed out")
        .and_then(|result| result);
    let completion = clipboard
        .call::<_, _, ()>("SelectionWriteDone", &(serial, transfer.is_ok()))
        .await;
    transfer?;
    completion.context("finish private clipboard transfer")?;
    Ok(())
}

async fn verify_owner(connection: &Connection, pid: u32) -> Result<OwnedUniqueName> {
    let dbus = DBusProxy::new(connection).await?;
    time::timeout(STREAM_TIMEOUT, async {
        loop {
            let remote = dbus
                .get_name_owner(BusName::try_from(REMOTE_DESKTOP)?)
                .await;
            let capture = dbus.get_name_owner(BusName::try_from(SCREEN_CAST)?).await;
            match (remote, capture) {
                (Ok(remote), Ok(capture)) => {
                    let owner_pid = dbus
                        .get_connection_unix_process_id(BusName::try_from(remote.as_str())?)
                        .await?;
                    ensure!(
                        remote == capture && owner_pid == pid,
                        "private D-Bus names are not owned by the started Mutter process"
                    );
                    return Ok(remote);
                }
                (Err(error), _) | (_, Err(error))
                    if !matches!(error, zbus::fdo::Error::NameHasNoOwner(_)) =>
                {
                    return Err(error.into());
                }
                _ => time::sleep(Duration::from_millis(40)).await,
            }
        }
    })
    .await
    .context("waiting for private Mutter D-Bus services")?
}

fn pair(properties: &HashMap<String, OwnedValue>, key: &str) -> Result<(i32, i32)> {
    let value = properties
        .get(key)
        .ok_or_else(|| anyhow!("Mutter stream has no {key}"))?;
    Ok(value.try_clone()?.try_into()?)
}

async fn send_key(remote: &Proxy<'_>, keysym: u32, modifiers: &[u32]) -> zbus::Result<()> {
    let mut pressed = Vec::with_capacity(modifiers.len());
    let mut main_attempted = false;
    let result = async {
        for code in modifiers {
            pressed.push(*code);
            remote
                .call::<_, _, ()>("NotifyKeyboardKeycode", &(*code, true))
                .await?;
        }
        main_attempted = true;
        remote
            .call::<_, _, ()>("NotifyKeyboardKeysym", &(keysym, true))
            .await
    }
    .await;
    let mut result = result;
    if main_attempted {
        let release = remote
            .call::<_, _, ()>("NotifyKeyboardKeysym", &(keysym, false))
            .await;
        result = result.and(release);
    }
    for code in pressed.into_iter().rev() {
        let release = remote
            .call::<_, _, ()>("NotifyKeyboardKeycode", &(code, false))
            .await;
        result = result.and(release);
    }
    result
}

async fn write_fd(fd: OwnedFd, data: &[u8]) -> Result<()> {
    let mut pipe = Sender::from_owned_fd(fd).context("open private clipboard pipe")?;
    pipe.write_all(data)
        .await
        .context("write private clipboard pipe")
}

#[cfg(test)]
mod tests;
