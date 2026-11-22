use std::{
    collections::{HashMap, VecDeque},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    net::unix::pipe::{self, Receiver},
    process::{Child, Command},
    sync::Mutex,
    time,
};
use zbus::{
    Connection,
    connection::Builder,
    fdo,
    object_server::SignalEmitter,
    zvariant::{self, OwnedObjectPath, OwnedValue},
};

use super::{CLIPBOARD, ClipboardApi, DBUS_TIMEOUT, LEGACY_CLIPBOARD, Mutter, TEXT_MIME_TYPES};

const SESSION_PATH: &str = "/org/gnome/Mutter/RemoteDesktop/Session/Test";
const TEST_TIMEOUT: Duration = Duration::from_millis(250);
const TEST_TEXT: &str = "한글\ncafé 🌱";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Selection {
    Ready,
    Silent,
    BlockedWriter,
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Enable,
    Select,
    Clear,
    Write(u32),
    Done(u32, bool, Vec<u8>),
    Disable,
}

type Events = Arc<Mutex<Vec<Event>>>;

struct FakeClipboard {
    script: VecDeque<Selection>,
    events: Events,
    enabled: bool,
    serial: u32,
    selection: Option<Selection>,
    pending: Option<(u32, Receiver)>,
}

#[zbus::interface(name = "org.gnome.Mutter.Clipboard")]
impl FakeClipboard {
    async fn enable(&mut self, _options: HashMap<String, OwnedValue>) -> fdo::Result<()> {
        if self.enabled {
            return Err(fdo::Error::Failed("Already enabled".into()));
        }
        self.enabled = true;
        self.events.lock().await.push(Event::Enable);
        Ok(())
    }

    async fn disable(&mut self) -> fdo::Result<()> {
        self.enabled = false;
        self.selection = None;
        self.pending = None;
        self.events.lock().await.push(Event::Disable);
        Ok(())
    }

    async fn set_selection(
        &mut self,
        options: HashMap<String, OwnedValue>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        self.select(options, &emitter, CLIPBOARD).await
    }

    #[zbus(signal)]
    async fn selection_transfer(
        emitter: &SignalEmitter<'_>,
        mime: &str,
        serial: u32,
    ) -> zbus::Result<()>;

    async fn selection_write(&mut self, serial: u32) -> fdo::Result<zvariant::OwnedFd> {
        self.events.lock().await.push(Event::Write(serial));
        if serial != self.serial || self.selection.is_none() {
            return Err(fdo::Error::Failed(format!("Invalid serial {serial}")));
        }

        let (sender, receiver) =
            pipe::pipe().map_err(|error| fdo::Error::IOError(error.to_string()))?;
        if self.selection == Some(Selection::BlockedWriter) {
            // Fill the actual pipe and keep the read end open without draining it.
            sender
                .writable()
                .await
                .map_err(|error| fdo::Error::IOError(error.to_string()))?;
            while sender.try_write(&[b'#'; 4096]).is_ok() {}
            time::sleep(TEST_TIMEOUT / 2).await;
        }
        self.pending = Some((serial, receiver));
        let fd = sender
            .into_nonblocking_fd()
            .map_err(|error| fdo::Error::IOError(error.to_string()))?;
        Ok(fd.into())
    }

    async fn selection_write_done(&mut self, serial: u32, success: bool) -> fdo::Result<()> {
        let (pending, mut receiver) = self.pending.take().expect("no opened transfer");
        assert_eq!(serial, pending);
        let mut bytes = Vec::new();
        time::timeout(Duration::from_secs(1), receiver.read_to_end(&mut bytes))
            .await
            .expect("writer should close the pipe before acknowledging it")
            .map_err(|error| fdo::Error::IOError(error.to_string()))?;
        self.events
            .lock()
            .await
            .push(Event::Done(serial, success, bytes));
        Ok(())
    }
}

impl FakeClipboard {
    async fn select(
        &mut self,
        options: HashMap<String, OwnedValue>,
        emitter: &SignalEmitter<'_>,
        api: ClipboardApi,
    ) -> fdo::Result<()> {
        if !self.enabled {
            return Err(fdo::Error::Failed("Clipboard not enabled".into()));
        }
        // The old selection can still be requested while the client replaces it.
        // By the time the signal is handled, Mutter has invalidated that serial.
        if self.selection.is_some() {
            self.serial += 1;
            emitter
                .emit(
                    api.interface,
                    "SelectionTransfer",
                    &(TEXT_MIME_TYPES[0], self.serial),
                )
                .await?;
        }
        let Some(mime_types) = options.get("mime-types") else {
            self.selection = None;
            self.events.lock().await.push(Event::Clear);
            return Ok(());
        };
        let offered: Vec<String> = mime_types
            .try_clone()
            .and_then(TryInto::try_into)
            .map_err(|error| fdo::Error::InvalidArgs(error.to_string()))?;
        assert_eq!(offered, TEXT_MIME_TYPES);
        self.events.lock().await.push(Event::Select);
        self.serial += 1;
        let next = self.script.pop_front().expect("unexpected SetSelection");
        self.selection = Some(next);
        if next != Selection::Silent {
            // A new cache request can reach the client before SetSelection returns.
            emitter
                .emit(
                    api.interface,
                    "SelectionTransfer",
                    &(TEXT_MIME_TYPES[0], self.serial),
                )
                .await?;
        }
        Ok(())
    }
}

struct FakeRemoteSession(Option<FakeClipboard>);

#[zbus::interface(name = "org.gnome.Mutter.RemoteDesktop.Session")]
impl FakeRemoteSession {
    fn notify_keyboard_keycode(&self, _code: u32, _pressed: bool) {}
    fn notify_keyboard_keysym(&self, _keysym: u32, _pressed: bool) {}

    async fn enable_clipboard(&mut self, options: HashMap<String, OwnedValue>) -> fdo::Result<()> {
        self.clipboard()?.enable(options).await
    }

    async fn disable_clipboard(&mut self) -> fdo::Result<()> {
        self.clipboard()?.disable().await
    }

    async fn set_selection(
        &mut self,
        options: HashMap<String, OwnedValue>,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        self.clipboard()?
            .select(options, &emitter, LEGACY_CLIPBOARD)
            .await
    }

    async fn selection_write(&mut self, serial: u32) -> fdo::Result<zvariant::OwnedFd> {
        self.clipboard()?.selection_write(serial).await
    }

    async fn selection_write_done(&mut self, serial: u32, success: bool) -> fdo::Result<()> {
        self.clipboard()?
            .selection_write_done(serial, success)
            .await
    }
}

impl FakeRemoteSession {
    fn clipboard(&mut self) -> fdo::Result<&mut FakeClipboard> {
        self.0
            .as_mut()
            .ok_or_else(|| fdo::Error::Failed("Unexpected use of the legacy clipboard".into()))
    }
}

struct DeniedClipboard;

#[zbus::interface(name = "org.gnome.Mutter.Clipboard")]
impl DeniedClipboard {
    fn enable(&self, _options: HashMap<String, OwnedValue>) -> fdo::Result<()> {
        Err(fdo::Error::AccessDenied(
            "Clipboard denied by test policy".into(),
        ))
    }
}

struct TestBus {
    mutter: Mutter,
    service: Connection,
    events: Events,
    _daemon: Child,
    _dir: tempfile::TempDir,
}

impl TestBus {
    async fn new(script: impl IntoIterator<Item = Selection>) -> Result<Self> {
        Self::with_clipboard(CLIPBOARD, script).await
    }

    async fn with_clipboard(
        api: ClipboardApi,
        script: impl IntoIterator<Item = Selection>,
    ) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let address = format!("unix:path={}", dir.path().join("bus").display());
        let mut daemon = Command::new("/usr/bin/dbus-daemon")
            .args([
                "--session",
                "--nofork",
                "--print-address=1",
                "--address",
                &address,
            ])
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("start the clipboard test's private dbus-daemon")?;
        let stdout = daemon.stdout.take().context("private bus stdout")?;
        let mut lines = BufReader::new(stdout).lines();
        time::timeout(Duration::from_secs(3), lines.next_line())
            .await??
            .context("private bus did not announce its address")?;

        let events = Events::default();
        let clipboard = FakeClipboard {
            script: script.into_iter().collect(),
            events: events.clone(),
            enabled: false,
            serial: 0,
            selection: None,
            pending: None,
        };
        let builder = Builder::address(address.as_str())?;
        let builder = if api == LEGACY_CLIPBOARD {
            builder.serve_at(SESSION_PATH, FakeRemoteSession(Some(clipboard)))?
        } else {
            builder
                .serve_at(SESSION_PATH, FakeRemoteSession(None))?
                .serve_at(SESSION_PATH, clipboard)?
        };
        let service = builder.build().await?;
        let connection = Builder::address(address.as_str())?
            .method_timeout(DBUS_TIMEOUT)
            .build()
            .await?;
        let remote_path = OwnedObjectPath::try_from(SESSION_PATH)?;
        let mutter = Mutter {
            connection,
            owner: service
                .unique_name()
                .context("private bus service name")?
                .clone(),
            stream_path: remote_path.clone(),
            remote_path,
            node: 0,
            clipboard_api: CLIPBOARD,
            clipboard_enabled: false,
            clipboard_task: None,
        };
        Ok(Self {
            _dir: dir,
            _daemon: daemon,
            service,
            mutter,
            events,
        })
    }

    async fn paste(&mut self, text: &str) -> Result<()> {
        self.mutter
            .paste_with_timeout(text.to_owned(), &[29, 42], TEST_TIMEOUT)
            .await
    }

    async fn stall_background_read(&self) -> Result<()> {
        let interface = self
            .service
            .object_server()
            .interface::<_, FakeClipboard>(SESSION_PATH)
            .await?;
        let serial = {
            let mut clipboard = interface.get_mut().await;
            assert_eq!(clipboard.serial, 1);
            clipboard.selection = Some(Selection::BlockedWriter);
            clipboard.serial += 1;
            clipboard.serial
        };
        FakeClipboard::selection_transfer(interface.signal_emitter(), TEXT_MIME_TYPES[0], serial)
            .await?;
        time::timeout(Duration::from_secs(1), async {
            while !self.events.lock().await.contains(&Event::Write(serial)) {
                time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .context("background transfer was not opened")?;
        Ok(())
    }

    async fn events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().await)
    }
}

#[tokio::test]
async fn legacy_clipboard_replaces_text_and_recovers_after_timeout() -> Result<()> {
    let mut bus = TestBus::with_clipboard(
        LEGACY_CLIPBOARD,
        [Selection::Ready, Selection::Silent, Selection::Ready],
    )
    .await?;
    bus.paste("previous clipboard").await?;
    let error = bus.paste("no receiver").await.unwrap_err();
    assert!(
        error.to_string().contains("no private clipboard transfer"),
        "{error:#}"
    );
    bus.paste(TEST_TEXT).await?;
    assert_eq!(
        bus.events().await,
        [
            Event::Enable,
            Event::Select,
            Event::Write(1),
            Event::Done(1, true, b"previous clipboard".to_vec()),
            Event::Clear,
            Event::Select,
            Event::Disable,
            Event::Enable,
            Event::Select,
            Event::Write(4),
            Event::Done(4, true, TEST_TEXT.as_bytes().to_vec()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn clipboard_access_denied_does_not_fall_back() -> Result<()> {
    let mut bus = TestBus::with_clipboard(LEGACY_CLIPBOARD, [Selection::Ready]).await?;
    bus.service
        .object_server()
        .at(SESSION_PATH, DeniedClipboard)
        .await?;
    let error = bus.paste(TEST_TEXT).await.unwrap_err();
    assert!(
        matches!(error.downcast_ref::<zbus::Error>(), Some(zbus::Error::MethodError(name, _, _))
        if name.as_str() == "org.freedesktop.DBus.Error.AccessDenied"),
        "{error:#}"
    );
    assert!(bus.events().await.is_empty());
    Ok(())
}

#[tokio::test]
async fn late_transfer_does_not_claim_the_next_paste() -> Result<()> {
    let mut bus = TestBus::new([Selection::Ready, Selection::Ready]).await?;
    bus.paste("previous clipboard").await?;
    bus.paste(TEST_TEXT).await?;
    assert_eq!(
        bus.events().await,
        [
            Event::Enable,
            Event::Select,
            Event::Write(1),
            Event::Done(1, true, b"previous clipboard".to_vec()),
            Event::Clear,
            Event::Select,
            Event::Write(3),
            Event::Done(3, true, TEST_TEXT.as_bytes().to_vec()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn missing_transfer_times_out_and_next_paste_recovers() -> Result<()> {
    let mut bus = TestBus::new([Selection::Silent, Selection::Ready]).await?;
    let error = bus.paste("no receiver").await.unwrap_err();
    assert!(
        error.to_string().contains("no private clipboard transfer"),
        "{error:#}"
    );
    assert_eq!(
        bus.events().await,
        [Event::Enable, Event::Select, Event::Disable]
    );

    bus.paste(TEST_TEXT).await?;
    assert_eq!(
        bus.events().await,
        [
            Event::Enable,
            Event::Select,
            Event::Write(2),
            Event::Done(2, true, TEST_TEXT.as_bytes().to_vec()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn stalled_fd_is_closed_and_acknowledged_before_recovery() -> Result<()> {
    let mut bus = TestBus::new([Selection::BlockedWriter, Selection::Ready]).await?;
    let error = bus.paste("blocked pipe").await.unwrap_err();
    let events = bus.events().await;
    assert!(
        matches!(&events[..], [
        Event::Enable, Event::Select, Event::Write(1), Event::Done(1, false, filler), Event::Disable,
    ] if !filler.is_empty() && filler.iter().all(|byte| *byte == b'#')),
        "{events:?}"
    );
    assert!(
        error.to_string().contains("clipboard transfer timed out"),
        "{error:#}"
    );

    bus.paste(TEST_TEXT).await?;
    assert_eq!(
        bus.events().await,
        [
            Event::Enable,
            Event::Select,
            Event::Write(2),
            Event::Done(2, true, TEST_TEXT.as_bytes().to_vec()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn replacing_clipboard_finishes_in_flight_background_transfer() -> Result<()> {
    let mut bus = TestBus::new([Selection::Ready, Selection::Ready]).await?;
    bus.paste("previous clipboard").await?;
    bus.events().await;
    bus.stall_background_read().await?;
    bus.paste(TEST_TEXT).await?;

    let events = bus.events().await;
    assert!(
        matches!(&events[..], [
        Event::Write(2),
        Event::Done(2, false, filler),
        Event::Clear,
        Event::Select,
        Event::Write(4),
        Event::Done(4, true, bytes),
    ] if !filler.is_empty() && filler.iter().all(|byte| *byte == b'#') && bytes == TEST_TEXT.as_bytes()),
        "{events:?}"
    );
    Ok(())
}
