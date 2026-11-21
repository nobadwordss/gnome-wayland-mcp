use std::sync::Arc;

use anyhow::{Result, anyhow, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use rmcp::{
    ServerHandler,
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    mutter::Mutter,
    runtime::{Capture, Process, Program, Runtime, Size},
};

#[derive(Clone)]
pub(crate) struct Server(Arc<Mutex<State>>);

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
    frame: Option<Uuid>,
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

#[derive(Clone, Copy, Default, Deserialize, JsonSchema)]
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
    fn evdev(self) -> u32 {
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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Text {
    /// UTF-8 text to paste (1 to 16,384 bytes, no NUL characters).
    text: String,
}

enum Action {
    Start,
    Stop,
    Status,
    Launch,
    Screenshot,
    Move(Point),
    Click(Click),
    Scroll(Scroll),
    Key(Key),
    Paste(Text),
}

impl Server {
    pub fn new(size: Size, program: Option<Program>) -> Self {
        Self(Arc::new(Mutex::new(State {
            size,
            program,
            desktop: None,
        })))
    }

    pub async fn shutdown(&self) {
        self.0.lock().await.stop().await;
    }

    async fn execute(&self, action: Action) -> CallToolResult {
        // A cancelled caller waiting for the lock has done nothing. Once started,
        // the operation owns the lock until its releases and cleanup finish.
        let mut state = self.0.clone().lock_owned().await;
        let result = tokio::spawn(async move { state.execute(action).await }).await;
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
        description = "Report the private desktop and configured application status.",
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
        description = "Move to a point in the most recent screenshot and click in the private desktop.",
        annotations(open_world_hint = false)
    )]
    async fn mouse_click(&self, Parameters(click): Parameters<Click>) -> CallToolResult {
        self.execute(Action::Click(click)).await
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
        description = "Paste literal UTF-8 text into an already focused private text field using the private clipboard.",
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
    async fn execute(&mut self, action: Action) -> Result<CallToolResult> {
        let size = self.size;
        match action {
            Action::Start => {
                if let Some(desktop) = &mut self.desktop {
                    if desktop.check().is_ok() {
                        return Ok(message(self.status()?));
                    }
                    self.stop().await;
                }
                let runtime = Runtime::start(size).await?;
                let mutter =
                    Mutter::connect(runtime.bus_address(), runtime.mutter_pid(), size).await?;
                let capture = runtime.start_capture(mutter.node, size).await?;
                mutter.prepare_input(size).await?;
                self.desktop = Some(Desktop {
                    application: None,
                    capture,
                    mutter,
                    runtime,
                    frame: None,
                });
                Ok(message(self.status()?))
            }
            Action::Stop => {
                self.stop().await;
                Ok(message(self.status()?))
            }
            Action::Status => Ok(message(self.status()?)),
            Action::Launch => {
                let program = self.program.as_ref().ok_or_else(|| {
                    anyhow!("configure an application in the server arguments after --")
                })?;
                let desktop = ready(&mut self.desktop)?;
                if !desktop.app_running()? {
                    desktop.frame = None;
                    desktop.application = Some(desktop.runtime.launch(program)?);
                }
                Ok(message(self.status()?))
            }
            Action::Screenshot => {
                let desktop = ready(&mut self.desktop)?;
                desktop.frame = None;
                let png = desktop.capture.screenshot().await?;
                let frame_id = Uuid::new_v4();
                desktop.frame = Some(frame_id);
                let metadata = json!({"frame_id":frame_id.to_string(), "width":size.width, "height":size.height});
                Ok(CallToolResult::success(vec![
                    ContentBlock::text(metadata.to_string()),
                    ContentBlock::image(STANDARD.encode(png), "image/png"),
                ]))
            }
            Action::Move(point) => {
                let desktop = ready(&mut self.desktop)?;
                desktop.begin_pointer(&point, size)?;
                dispatched(desktop.mutter.move_pointer(point.x, point.y).await)
            }
            Action::Click(Click { point, button }) => {
                let desktop = ready(&mut self.desktop)?;
                desktop.begin_pointer(&point, size)?;
                let result = async {
                    desktop.mutter.move_pointer(point.x, point.y).await?;
                    desktop.mutter.click(button.evdev()).await
                }
                .await;
                dispatched(result)
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
                let desktop = ready(&mut self.desktop)?;
                desktop.begin_pointer(&point, size)?;
                let result = async {
                    desktop.mutter.move_pointer(point.x, point.y).await?;
                    desktop.mutter.scroll(horizontal, vertical).await
                }
                .await;
                dispatched(result)
            }
            Action::Key(key) => {
                let (keysym, modifiers) = key.validate()?;
                let desktop = ready(&mut self.desktop)?;
                desktop.frame = None;
                dispatched(desktop.mutter.key(keysym, &modifiers).await)
            }
            Action::Paste(Text { text }) => {
                ensure!(
                    (1..=16_384).contains(&text.len()) && !text.contains('\0'),
                    "text must be 1 to 16,384 UTF-8 bytes without NUL characters"
                );
                let desktop = ready(&mut self.desktop)?;
                desktop.frame = None;
                dispatched(desktop.mutter.paste(text).await)
            }
        }
    }

    fn status(&mut self) -> Result<Value> {
        let state = match &mut self.desktop {
            Some(desktop) => {
                if desktop.check().is_err() {
                    "failed"
                } else if desktop.app_running()? {
                    "application_running"
                } else {
                    "desktop_running"
                }
            }
            None => "stopped",
        };
        Ok(
            json!({ "status": state, "mode": "headless", "application_configured": self.program.is_some(),
            "width": self.size.width, "height": self.size.height }),
        )
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
    fn check(&self) -> Result<()> {
        self.runtime.check()?;
        self.capture.check()
    }

    fn app_running(&self) -> Result<bool> {
        match &self.application {
            Some(app) => app.running(),
            None => Ok(false),
        }
    }

    fn begin_pointer(&mut self, point: &Point, size: Size) -> Result<()> {
        let frame = Uuid::parse_str(&point.frame_id).ok();
        ensure!(
            self.frame.is_some() && frame == self.frame,
            "frame_id is stale or unknown; take a new screenshot"
        );
        ensure!(
            point.x < size.width && point.y < size.height,
            "point is outside the screenshot"
        );
        self.frame = None;
        Ok(())
    }
}

fn ready(desktop: &mut Option<Desktop>) -> Result<&mut Desktop> {
    let desktop = desktop
        .as_mut()
        .ok_or_else(|| anyhow!("start the private desktop first"))?;
    desktop.check()?;
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
