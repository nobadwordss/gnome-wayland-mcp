use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Result, anyhow, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rmcp::{
    ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    sync::Mutex,
    time::{Instant, sleep, sleep_until, timeout_at},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    mutter::Mutter,
    runtime::{Capture, CaptureHealth, GTK_DOUBLE_CLICK_TIME, Process, Program, Runtime, Size},
};

#[derive(Clone)]
pub(crate) struct Server {
    state: Arc<Mutex<State>>,
    stopping: CancellationToken,
}

struct State {
    size: Size,
    program: Option<Program>,
    desktop: Option<Desktop>,
}

struct Desktop {
    application: Option<Process>,
    capture: Capture,
    mutter: Mutter,
    runtime: Runtime,
    double_click_ready_at: HashMap<Button, Instant>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ApplicationStatus {
    NotConfigured,
    NotStarted,
    Running,
    Exited {
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        signal: Option<i32>,
    },
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Point {
    /// Opaque ID from the latest screenshot.
    frame_id: String,
    /// Horizontal pixel from the left edge of the screenshot.
    x: u32,
    /// Vertical pixel from the top edge of the screenshot.
    y: u32,
}

#[derive(Clone, Copy, Default, Deserialize, Eq, Hash, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Button {
    #[default]
    Left,
    Middle,
    Right,
}

impl Button {
    fn evdev(self) -> i32 {
        match self {
            Self::Left => 272,
            Self::Right => 273,
            Self::Middle => 274,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Click {
    point: Point,
    #[serde(default)]
    button: Button,
    /// Number of clicks at the same point. Use 1 (default) or 2.
    #[serde(default = "one_click")]
    #[schemars(range(min = 1, max = 2))]
    count: u8,
}

fn one_click() -> u8 {
    1
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Pixel {
    x: u32,
    y: u32,
}

impl Pixel {
    fn check(self, size: Size) -> Result<()> {
        ensure!(
            self.x < size.width && self.y < size.height,
            "point is outside the screenshot"
        );
        Ok(())
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Drag {
    /// Opaque ID from the latest screenshot, shared by both endpoints.
    frame_id: String,
    /// Pixel where the button is pressed.
    start: Pixel,
    /// Pixel where the button is released.
    end: Pixel,
    #[serde(default)]
    button: Button,
}

#[derive(Clone, Copy)]
enum ButtonGesture {
    Click,
    DoubleClick,
    Drag(Pixel),
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Scroll {
    point: Point,
    /// Wheel steps; positive goes down. Range -20 to 20.
    #[serde(default)]
    vertical: i32,
    /// Wheel steps; positive goes right. Range -20 to 20.
    #[serde(default)]
    horizontal: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Super,
}

impl Modifier {
    const fn evdev(self) -> u32 {
        match self {
            Self::Ctrl => 29,
            Self::Alt => 56,
            Self::Shift => 42,
            Self::Super => 125,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Key {
    /// One lowercase ASCII letter, digit, or named key such as Enter, Escape, Left, or F5.
    key: String,
    /// Modifiers held while pressing the key. Each may appear once.
    #[serde(default)]
    modifiers: Vec<Modifier>,
}

#[derive(Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PasteShortcut {
    #[default]
    CtrlV,
    CtrlShiftV,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Text {
    /// UTF-8 text to paste (1 to 16,384 bytes, no NUL characters).
    text: String,
    /// Paste shortcut supported by the focused app. Omit or pass null for Ctrl+V.
    paste_shortcut: Option<PasteShortcut>,
}

enum Action {
    Start,
    Stop,
    Status,
    Launch,
    Screenshot,
    Move(Point),
    Click(Click),
    Drag(Drag),
    Scroll(Scroll),
    Key(Key),
    Paste(Text),
}

impl Server {
    pub fn new(size: Size, program: Option<Program>, stopping: CancellationToken) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                size,
                program,
                desktop: None,
            })),
            stopping,
        }
    }

    pub async fn shutdown(&self) {
        self.stopping.cancel();
        self.state.lock().await.stop().await;
    }

    async fn execute(&self, action: Action) -> CallToolResult {
        // A cancelled caller waiting for the lock has done nothing. Once started,
        // the operation owns the lock until its releases and cleanup finish.
        let Some(mut state) = self
            .stopping
            .run_until_cancelled(self.state.clone().lock_owned())
            .await
        else {
            return CallToolResult::error(vec![ContentBlock::text(
                "Private desktop server is shutting down.",
            )]);
        };
        let stopping = self.stopping.clone();
        let result = tokio::spawn(async move {
            ensure!(
                !stopping.is_cancelled(),
                "private desktop server is shutting down"
            );
            state.execute(action, &stopping).await
        })
        .await;
        match result {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => CallToolResult::error(vec![ContentBlock::text(format!("{error:#}"))]),
            Err(_) => CallToolResult::error(vec![ContentBlock::text(
                "Operation interrupted. Check the private desktop status.",
            )]),
        }
    }
}

#[tool_router]
impl Server {
    #[tool(
        description = "Start a private headless GNOME Wayland desktop with one virtual monitor.",
        annotations(
            open_world_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn desktop_start(&self) -> CallToolResult {
        self.execute(Action::Start).await
    }

    #[tool(
        description = "Stop the private desktop and its running application. Unsaved app data may be lost.",
        annotations(
            open_world_hint = false,
            destructive_hint = true,
            idempotent_hint = true
        )
    )]
    async fn desktop_stop(&self) -> CallToolResult {
        self.execute(Action::Stop).await
    }

    #[tool(
        description = "Report the private desktop, application and capture status, including the app's exit code or signal after termination.",
        annotations(open_world_hint = false, read_only_hint = true)
    )]
    async fn desktop_status(&self) -> CallToolResult {
        self.execute(Action::Status).await
    }

    #[tool(
        description = "Launch the application configured on the server command line inside the private desktop.",
        annotations(
            open_world_hint = false,
            destructive_hint = false,
            idempotent_hint = true
        )
    )]
    async fn launch_app(&self) -> CallToolResult {
        self.execute(Action::Launch).await
    }

    #[tool(
        description = "Capture the private monitor as a PNG. Use its frame_id and image pixels for one pointer action.",
        annotations(open_world_hint = false, read_only_hint = true)
    )]
    async fn screenshot(&self) -> CallToolResult {
        self.execute(Action::Screenshot).await
    }

    #[tool(
        description = "Move the private pointer to a point from the most recent screenshot.",
        annotations(open_world_hint = false, destructive_hint = false)
    )]
    async fn mouse_move(&self, Parameters(point): Parameters<Point>) -> CallToolResult {
        self.execute(Action::Move(point)).await
    }

    #[tool(
        description = "Click a point in the latest screenshot. Set count to 2 to double-click with one frame ID.",
        annotations(open_world_hint = false)
    )]
    async fn mouse_click(&self, Parameters(click): Parameters<Click>) -> CallToolResult {
        self.execute(Action::Click(click)).await
    }

    #[tool(
        description = "Drag from start to end in the latest screenshot using one frame ID. The server holds and releases the button.",
        annotations(open_world_hint = false)
    )]
    async fn mouse_drag(&self, Parameters(drag): Parameters<Drag>) -> CallToolResult {
        self.execute(Action::Drag(drag)).await
    }

    #[tool(
        description = "Move to a point in the most recent screenshot and scroll in the private desktop.",
        annotations(open_world_hint = false)
    )]
    async fn mouse_scroll(&self, Parameters(scroll): Parameters<Scroll>) -> CallToolResult {
        self.execute(Action::Scroll(scroll)).await
    }

    #[tool(
        description = "Press and release a key or key combination at the current private keyboard focus.",
        annotations(open_world_hint = false)
    )]
    async fn press_key(&self, Parameters(key): Parameters<Key>) -> CallToolResult {
        self.execute(Action::Key(key)).await
    }

    #[tool(
        description = "Paste literal UTF-8 text into a focused field using the private clipboard. Verify insertion with a fresh screenshot before pressing Enter or pasting again.",
        annotations(open_world_hint = false)
    )]
    async fn type_text(&self, Parameters(text): Parameters<Text>) -> CallToolResult {
        self.execute(Action::Paste(text)).await
    }
}

#[tool_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
            .with_instructions("Start the private desktop, launch the configured app, and use screenshots to ground pointer input. Every new screenshot, launch, or input invalidates earlier frame IDs. Verify effects with a fresh screenshot. Only applications in this headless desktop receive input.")
    }
}

impl State {
    async fn execute(
        &mut self,
        action: Action,
        stopping: &CancellationToken,
    ) -> Result<CallToolResult> {
        let size = self.size;
        match action {
            Action::Start => {
                if let Some(desktop) = self.desktop.as_mut().filter(|d| d.runtime.check().is_ok()) {
                    desktop.recover_capture(size, stopping).await?;
                } else {
                    self.stop().await;
                    self.desktop = Some(Desktop::start(size, stopping).await?);
                }
                Ok(message(self.status(false).await?))
            }
            Action::Stop => {
                self.stop().await;
                Ok(message(self.status(false).await?))
            }
            Action::Status => Ok(message(self.status(true).await?)),
            Action::Launch => {
                let program = self.program.as_ref().ok_or_else(|| {
                    anyhow!("configure an application in the server arguments after --")
                })?;
                let desktop = running_desktop(&mut self.desktop)?;
                if !desktop.app_running()? {
                    desktop.capture.frame_id = None;
                    desktop.application = Some(desktop.runtime.launch(program)?);
                }
                Ok(message(self.status(false).await?))
            }
            Action::Screenshot => {
                let desktop = running_desktop(&mut self.desktop)?;
                desktop.capture.frame_id = None;
                desktop.capture.wait_for_frame().await;
                desktop
                    .recover_capture(size, stopping)
                    .await?
                    .require_ready()?;
                let png = desktop.capture.latest_frame()?;
                let image = ContentBlock::image(STANDARD.encode(png), "image/png");
                desktop
                    .capture
                    .require_uninterrupted(&desktop.runtime)
                    .await?;
                let frame_id = Uuid::new_v4();
                desktop.capture.frame_id = Some(frame_id);
                let metadata = json!({"frame_id":frame_id.to_string(), "width":size.width, "height":size.height});
                Ok(CallToolResult::success(vec![
                    ContentBlock::text(metadata.to_string()),
                    image,
                ]))
            }
            Action::Move(point) => {
                let desktop = running_desktop(&mut self.desktop)?;
                desktop.begin_pointer(&point, size).await?;
                dispatched(desktop.move_pointer(point.x, point.y).await)
            }
            Action::Click(Click {
                point,
                button,
                count,
            }) => {
                let gesture = match count {
                    1 => ButtonGesture::Click,
                    2 => ButtonGesture::DoubleClick,
                    _ => bail!("count must be 1 or 2"),
                };
                running_desktop(&mut self.desktop)?
                    .button_gesture(&point, size, button, gesture, stopping)
                    .await
            }
            Action::Drag(Drag {
                frame_id,
                start,
                end,
                button,
            }) => {
                end.check(size)?;
                ensure!(
                    start.x != end.x || start.y != end.y,
                    "drag endpoints must differ"
                );
                let point = Point {
                    frame_id,
                    x: start.x,
                    y: start.y,
                };
                running_desktop(&mut self.desktop)?
                    .button_gesture(&point, size, button, ButtonGesture::Drag(end), stopping)
                    .await
            }
            Action::Scroll(Scroll {
                point,
                horizontal,
                vertical,
            }) => {
                ensure!(
                    (-20..=20).contains(&horizontal)
                        && (-20..=20).contains(&vertical)
                        && (horizontal != 0 || vertical != 0),
                    "scroll requires a nonzero step count from -20 to 20"
                );
                let desktop = running_desktop(&mut self.desktop)?;
                desktop.begin_pointer(&point, size).await?;
                let result = async {
                    desktop.move_pointer(point.x, point.y).await?;
                    desktop
                        .capture
                        .require_uninterrupted(&desktop.runtime)
                        .await?;
                    desktop.mutter.scroll(horizontal, vertical).await
                }
                .await;
                dispatched(result)
            }
            Action::Key(key) => {
                let (keysym, modifiers) = key.validate()?;
                let desktop = running_desktop(&mut self.desktop)?;
                desktop.begin_keyboard_input()?;
                dispatched(desktop.mutter.key(keysym, &modifiers).await)
            }
            Action::Paste(Text {
                text,
                paste_shortcut,
            }) => {
                ensure!(
                    (1..=16_384).contains(&text.len()) && !text.contains('\0'),
                    "text must be 1 to 16,384 UTF-8 bytes without NUL characters"
                );
                let desktop = running_desktop(&mut self.desktop)?;
                desktop.begin_keyboard_input()?;
                let modifiers: &[u32] = match paste_shortcut.unwrap_or_default() {
                    PasteShortcut::CtrlV => &[Modifier::Ctrl.evdev()],
                    PasteShortcut::CtrlShiftV => &[Modifier::Ctrl.evdev(), Modifier::Shift.evdev()],
                };
                dispatched(desktop.mutter.paste(text, modifiers).await)
            }
        }
    }

    async fn status(&self, detailed: bool) -> Result<Value> {
        let application = self.application_status()?;
        let state = match &self.desktop {
            Some(desktop) if desktop.runtime.check().is_err() => "failed",
            Some(_) if matches!(application, ApplicationStatus::Running) => "application_running",
            Some(_) => "desktop_running",
            None => "stopped",
        };
        let mut status = json!({ "status": state, "mode": "headless", "application_configured": self.program.is_some(),
            "width": self.size.width, "height": self.size.height });
        if detailed {
            status["application"] = json!(application);
            status["capture"] = match &self.desktop {
                None => json!({"status": "stopped"}),
                Some(_) if state == "failed" => json!({"status": "unavailable"}),
                Some(desktop) => match desktop.capture.health(&desktop.runtime).await {
                    Ok(health) => json!({"status": health}),
                    Err(error) => json!({"status": "unknown", "error": format!("{error:#}")}),
                },
            };
        }
        Ok(status)
    }

    fn application_status(&self) -> Result<ApplicationStatus> {
        let application = self
            .desktop
            .as_ref()
            .and_then(|desktop| desktop.application.as_ref());
        let Some(application) = application else {
            return Ok(if self.program.is_some() {
                ApplicationStatus::NotStarted
            } else {
                ApplicationStatus::NotConfigured
            });
        };
        Ok(match application.exit_state()? {
            None => ApplicationStatus::Running,
            Some(status) => ApplicationStatus::Exited {
                exit_code: status.exit_status(),
                signal: status.terminating_signal(),
            },
        })
    }

    async fn stop(&mut self) {
        if let Some(mut desktop) = self.desktop.take() {
            if let Some(app) = &mut desktop.application {
                app.stop().await;
            }
            desktop.capture.stop().await;
            desktop.mutter.stop().await;
            desktop.runtime.stop().await;
        }
    }
}

impl Desktop {
    async fn start(size: Size, stopping: &CancellationToken) -> Result<Self> {
        let mut runtime = Runtime::start(size, stopping).await?;
        let startup = async {
            let mutter = stopping
                .run_until_cancelled(Mutter::connect(
                    &runtime.bus_path(),
                    runtime.mutter_pid(),
                    size,
                ))
                .await
                .ok_or_else(|| anyhow!("desktop startup cancelled"))??;
            let mut capture = runtime.start_capture(mutter.node, size, stopping).await?;
            let prepared = stopping
                .run_until_cancelled(mutter.prepare_input(size))
                .await
                .unwrap_or_else(|| Err(anyhow!("desktop startup cancelled")));
            if prepared.is_err() || stopping.is_cancelled() {
                capture.stop().await;
                prepared?;
                bail!("desktop startup cancelled");
            }
            Ok((mutter, capture))
        }
        .await;
        match startup {
            Ok((mutter, capture)) => Ok(Self {
                application: None,
                capture,
                mutter,
                runtime,
                double_click_ready_at: HashMap::new(),
            }),
            Err(error) => {
                runtime.stop().await;
                Err(error)
            }
        }
    }

    async fn recover_capture(
        &mut self,
        size: Size,
        stopping: &CancellationToken,
    ) -> Result<CaptureHealth> {
        let health = self.capture.health(&self.runtime).await?;
        if health != CaptureHealth::RecoveryNeeded {
            return Ok(health);
        }
        // A disconnected reader can still hold PipeWire buffers until it is reaped.
        self.capture.frame_id = None;
        self.capture.stop().await;
        self.capture = self
            .runtime
            .start_capture(self.mutter.node, size, stopping)
            .await?;
        Ok(CaptureHealth::Ready)
    }

    fn app_running(&self) -> Result<bool> {
        match &self.application {
            Some(app) => app.running(),
            None => Ok(false),
        }
    }

    fn begin_keyboard_input(&mut self) -> Result<()> {
        ensure!(
            !self.capture.is_closed()?,
            "private capture stream closed; take a new screenshot"
        );
        self.capture.frame_id = None;
        Ok(())
    }

    async fn begin_pointer(&mut self, point: &Point, size: Size) -> Result<()> {
        let frame = Uuid::parse_str(&point.frame_id).ok();
        ensure!(
            self.capture.frame_id.is_some() && frame == self.capture.frame_id,
            "frame_id is stale or unknown; take a new screenshot"
        );
        Pixel {
            x: point.x,
            y: point.y,
        }
        .check(size)?;
        self.capture.frame_id = None;
        self.capture.require_uninterrupted(&self.runtime).await
    }

    async fn move_pointer(&self, x: u32, y: u32) -> Result<()> {
        self.mutter
            .move_pointer(x, y, Some(self.capture.link_interrupted()))
            .await
    }

    async fn button_gesture(
        &mut self,
        point: &Point,
        size: Size,
        button: Button,
        gesture: ButtonGesture,
        stopping: &CancellationToken,
    ) -> Result<CallToolResult> {
        let mut started = false;
        let result = async {
            self.begin_pointer(point, size).await?;
            if matches!(gesture, ButtonGesture::DoubleClick)
                && let Some(&ready_at) = self.double_click_ready_at.get(&button)
                && ready_at > Instant::now()
            {
                // GTK may still be counting a previous click on this button,
                // even at another position. Separate the two native gestures.
                let link = self.capture.link_interrupted();
                tokio::select! {
                    biased;
                    _ = stopping.cancelled() => bail!("private desktop server is shutting down"),
                    _ = link.cancelled() => bail!("private capture was interrupted; take a new screenshot"),
                    _ = sleep_until(ready_at) => {}
                }
                self.capture.require_uninterrupted(&self.runtime).await?;
            }
            self.move_pointer(point.x, point.y).await?;
            self.capture.require_uninterrupted(&self.runtime).await?;
            let link = self.capture.link_interrupted();
            let button = button.evdev();
            // Avoid sending an unrelated second click if the compositor or graph query stalls.
            let double_click_deadline = Instant::now() + Duration::from_millis(200);
            self.mutter
                .with_button_down(button, link.cancelled(), &mut started, async {
                    if let ButtonGesture::Drag(end) = gesture {
                        self.drag_path(point, end, stopping).await?;
                    }
                    Ok(())
                })
                .await?;
            if matches!(gesture, ButtonGesture::DoubleClick) {
                sleep(Duration::from_millis(40)).await;
                ensure!(
                    !stopping.is_cancelled(),
                    "private desktop server is shutting down"
                );
                self.capture.require_uninterrupted(&self.runtime).await?;
                ensure!(
                    Instant::now() < double_click_deadline,
                    "double-click interval elapsed before second press"
                );
                let interrupt = async {
                    let _ = timeout_at(double_click_deadline, link.cancelled()).await;
                };
                self.mutter
                    .with_button_down(button, interrupt, &mut started, std::future::ready(Ok(())))
                    .await?;
            }
            // Observe any disconnect still in transit before acknowledging the gesture.
            self.capture.require_uninterrupted(&self.runtime).await
        }
        .await;
        if started {
            self.double_click_ready_at
                .insert(button, Instant::now() + GTK_DOUBLE_CLICK_TIME);
        }
        if let Err(error) = result {
            let (not_sent, uncertain) = match gesture {
                ButtonGesture::Click => {
                    ("Click was not sent", "The click may have been delivered.")
                }
                ButtonGesture::DoubleClick => (
                    "Double-click was not sent",
                    "The double-click may have been partially delivered.",
                ),
                ButtonGesture::Drag(_) => (
                    "Drag was not sent",
                    "The drag may have been partially delivered.",
                ),
            };
            if started {
                bail!(
                    "{uncertain} Do not retry it. Take a new screenshot to check the result.\n{error:#}"
                );
            }
            return Err(error.context(not_sent));
        }
        Ok(message(json!({"status":"dispatched"})))
    }

    async fn drag_path(
        &self,
        start: &Point,
        end: Pixel,
        stopping: &CancellationToken,
    ) -> Result<()> {
        let distance = start.x.abs_diff(end.x).max(start.y.abs_diff(end.y));
        let steps = distance.min(12);
        for step in 1..=steps {
            sleep(Duration::from_millis(12)).await;
            ensure!(
                !stopping.is_cancelled(),
                "private desktop server is shutting down"
            );
            self.capture.observed_health()?.require_ready()?;
            let interpolate = |a: u32, b: u32| (a * (steps - step) + b * step + steps / 2) / steps;
            self.move_pointer(interpolate(start.x, end.x), interpolate(start.y, end.y))
                .await?;
        }
        Ok(())
    }
}

fn running_desktop(desktop: &mut Option<Desktop>) -> Result<&mut Desktop> {
    let desktop = desktop
        .as_mut()
        .ok_or_else(|| anyhow!("start the private desktop first"))?;
    desktop.runtime.check()?;
    Ok(desktop)
}

impl Key {
    fn validate(&self) -> Result<(u32, Vec<u32>)> {
        ensure!(
            self.modifiers.len() <= 4,
            "at most four distinct modifiers are supported"
        );
        let mut codes = Vec::new();
        for modifier in &self.modifiers {
            let code = modifier.evdev();
            ensure!(!codes.contains(&code), "duplicate modifier");
            codes.push(code);
        }
        let value = match self.key.as_str() {
            "Enter" => 0xff0d,
            "Tab" => 0xff09,
            "Escape" => 0xff1b,
            "Backspace" => 0xff08,
            "Delete" => 0xffff,
            "Insert" => 0xff63,
            "Space" => 0x20,
            "Home" => 0xff50,
            "End" => 0xff57,
            "PageUp" => 0xff55,
            "PageDown" => 0xff56,
            "Left" => 0xff51,
            "Up" => 0xff52,
            "Right" => 0xff53,
            "Down" => 0xff54,
            key if key.len() == 1
                && (key.as_bytes()[0].is_ascii_lowercase()
                    || key.as_bytes()[0].is_ascii_digit()) =>
            {
                u32::from(key.as_bytes()[0])
            }
            key => match key
                .strip_prefix('F')
                .and_then(|number| number.parse::<u32>().ok())
            {
                Some(number @ 1..=12) => 0xffbd + number,
                _ => bail!(
                    "unsupported key; use a lowercase ASCII letter, digit or a supported named key"
                ),
            },
        };
        Ok((value, codes))
    }
}

fn dispatched(result: Result<()>) -> Result<CallToolResult> {
    result.map_err(|error| anyhow!("Input may have been partially dispatched. Check the private desktop before retrying. {error:#}"))?;
    Ok(message(json!({"status":"dispatched"})))
}

fn message(value: Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(value.to_string())])
}
