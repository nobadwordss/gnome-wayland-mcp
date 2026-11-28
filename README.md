# GNOME Wayland MCP

Test native Linux GUI apps with an MCP agent on a separate GNOME Wayland display. The agent can launch a configured app, capture screenshots, click, and type while you keep using your regular desktop.

[Install for Codex](#install-for-codex) · [Other MCP clients and app examples](docs/setup.md) · [Ubuntu and Fedora verification](docs/verification.md)

![Codex opens GNOME Text Editor, clicks New Document, pastes a multiline checklist with an emoji, and finds a match with Ctrl+F](docs/gnome-text-editor-demo.gif)

*Actual v0.2.2 screenshots from the private display, shown alongside the MCP calls that produced them.* [Full-size screenshot from the original Codex test](docs/codex-headless-demo.png).

## Quick start

### Requirements

Use a Linux host with Mutter, D-Bus, PipeWire (including `pw-dump`), WirePlumber 0.5 or later, and GStreamer installed under `/usr/bin`. The GStreamer `pipewiresrc` element must support `on-disconnect=error`. On Fedora, install any missing runtime dependencies with the following command.

```sh
sudo dnf install mutter dbus-daemon pipewire pipewire-utils wireplumber gstreamer1 \
  pipewire-gstreamer gstreamer1-plugins-base gstreamer1-plugins-good
```

On Ubuntu 26.04 LTS, install the distribution packages below. Mutter is available in Ubuntu's Universe repository.

```sh
sudo apt update
sudo apt install curl mutter dbus-daemon pipewire pipewire-bin wireplumber libgl1-mesa-dri \
  gstreamer1.0-tools gstreamer1.0-pipewire gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good
```

The editor demo uses the optional `gnome-text-editor` package. You can use another native Wayland app. Ubuntu 24.04 LTS ships WirePlumber 0.4, below the required version.

### Install for Codex

On Linux x86_64 with the Codex CLI on your `PATH`, run `bash scripts/install-codex.sh` from this checkout as your regular user. The installer selects GNOME Text Editor if installed. Otherwise you get a desktop you can start and capture without an app.

To choose an app on the first run, pass its absolute executable path and arguments. For example, on a host with native Firefox installed at `/usr/bin/firefox`, use this command.

```sh
bash scripts/install-codex.sh -- /usr/bin/firefox --no-remote --new-instance about:blank
```

Pass `--` alone to explicitly register a desktop without an app. For the published installer and editor demo, you can also [download the installer](docs/setup.md#download-the-codex-installer).

The installer verifies the archive's SHA-256 checksum and the executable's version, then installs v0.2.8 under `~/.local/share/gnome-wayland-mcp/v0.2.8/`. The installer keeps existing Codex entries. To add another app, register the binary under a second name. [Build and registration commands](docs/setup.md)

## Try it

1. Open a new chat in your MCP client and call `desktop_start`.
2. If you configured an app, call `launch_app`. Call `screenshot` and check the PNG. If the window is still opening, take another screenshot. A desktop without an app can also return a screenshot.
3. For the editor demo, open a document and enter “Hello from the private desktop” and “한글 입력 확인” on separate lines. Take a fresh screenshot to verify both lines.
4. Call `desktop_stop`, then confirm that `desktop_status` reports `stopped`. The app's temporary home is removed at shutdown.

<a id="ptyxis-example"></a>
For manual setup, see the [binary download](docs/setup.md#download-the-linux-x86_64-binary), [Claude Code](docs/setup.md#claude-code), [Gemini CLI](docs/setup.md#gemini-cli), [optional Codex skill](docs/setup.md#optional-codex-skill), and [Firefox](docs/setup.md#firefox-example) or [Ptyxis](docs/setup.md#ptyxis-example) examples.

## MCP tools

| Tool | What it does |
| --- | --- |
| `desktop_start` | Starts a private Mutter desktop with one virtual monitor. |
| `desktop_status` | Reports desktop, application and capture status (see below). |
| `launch_app` | Starts the app specified in the server's arguments. |
| `screenshot` | Returns a PNG and its `frame_id`, `width`, and `height`. |
| `mouse_move` | Moves the pointer to `{frame_id, x, y}`. |
| `mouse_click` | Clicks `{point: {frame_id, x, y}, button}`. The button defaults to `left`. Pass `count: 2` for a double-click or omit it for a single click. |
| `mouse_drag` | Drags from `start` to `end` in a straight line using one `frame_id`. Both endpoints use screenshot pixels. The optional `button` defaults to `left`. |
| `mouse_scroll` | Scrolls at `{point: {frame_id, x, y}, vertical, horizontal}`. Positive steps move down or right. |
| `press_key` | Sends `{key, modifiers}`. Supports lowercase ASCII letters, digits, and named keys such as `Enter` and `Left`. |
| `type_text` | Pastes up to 16 KiB of UTF-8 from `{text}` into the focused editable field. Defaults to Ctrl+V. |
| `desktop_stop` | Stops the desktop and the running app. |

`desktop_status` includes an `application` object. Its `status` is `not_configured` when the server has no app command, `not_started` when a configured app has yet to launch in the current session, `running` while the app process is alive, or `exited` when it has terminated. For `exited`, the object includes `exit_code` for a process exit (including `0`) or `signal` for the Linux signal number. A successful relaunch replaces the previous process status, and stopping the desktop resets it. Use a screenshot to verify that a running process has opened its window.

`desktop_status` also reports `capture.status`. A running desktop can have a `ready`, `paused`, or `recovery_needed` capture. A stopped desktop reports `stopped`. If a desktop service has failed, capture is `unavailable`. If the server cannot inspect the capture, it reports `unknown` with an error. Checking status neither starts nor replaces a capture.

Screenshot coordinates start at the top-left pixel. Each pointer action uses the current `frame_id`. A new screenshot, app launch, or input invalidates earlier IDs. A `dispatched` response confirms delivery to Mutter. For `type_text`, Mutter also acknowledges a clipboard transfer and can request the text for its own clipboard cache. Use a fresh screenshot to verify that the app inserted the text before pressing Enter or replacing the clipboard with another `type_text` call.

For a word selection, call `mouse_click` with `{"point":{"frame_id":"…","x":320,"y":140},"count":2}`. For a range, call `mouse_drag` with `{"frame_id":"…","start":{"x":320,"y":140},"end":{"x":480,"y":140}}`. Choose two distinct pixels from the same screenshot. A fresh screenshot shows the resulting selection. The [GTK gesture regression](tests/pointer_gestures.py) checks the selected text and button release after a disconnect on Ubuntu and Fedora.

Pass `{"text":"Hello","paste_shortcut":"ctrl_shift_v"}` for apps whose paste shortcut is Ctrl+Shift+V. Omit `paste_shortcut`, pass `null`, or pass `"ctrl_v"` to use Ctrl+V.

## Session behavior

The private desktop owns its Wayland socket, D-Bus, PipeWire, keyboard focus, and clipboard. Screenshots and input apply to apps launched inside that desktop. The app gets a temporary home and profile, which are removed when the session ends. Save any files you want to retain to an explicit persistent path. After a successful `type_text` call, the text remains available to apps on the private clipboard until another paste replaces it or the session stops. Mutter can also retain a cached copy after a paste error. Check the app before retrying an error that reports possible dispatch.

The runtime uses the installed Mutter executable directly. The private display and input are separate from your regular session. Apps run with the invoking user's OS permissions and can access files and the network as that user.

Each MCP server process keeps at most one private desktop. Repeated `desktop_start` calls reuse a healthy desktop. Separate server processes can each own a desktop, and an idle server keeps its desktop until it receives a stop or shutdown request.

`screenshot` and `desktop_start` can replace a disconnected capture on the same Mutter stream. The first recovered screenshot uses a complete PNG from the new capture. The desktop and app keep running. A paused capture can recover after it resumes. `launch_app` uses the running desktop even while capture needs recovery. If the capture reader has closed, take a new screenshot before sending keys or pasting.

The server checks the original capture link's serial and active state before issuing a `frame_id` or pressing a button. It watches that link with `pw-dump --monitor`. If it detects capture loss before the first press starts, it rejects the gesture. If it detects loss after a press may have reached Mutter, it stops the remaining gesture, attempts the matching release and reports possible delivery. If a gesture reports possible delivery, inspect a fresh screenshot before taking further action.

Starting with v0.2.8, an explicit double-click starts a new click sequence. If the same button was used recently, the server waits out the remainder of the private GTK 4 desktop's 400 ms recognition interval before the first press. Once the first press starts, the server allows up to 200 ms to start the second.

Use `desktop_stop` to release the desktop while the MCP client is open, then check for `stopped` with `desktop_status`. The server stops its app and desktop services and removes the temporary runtime directory. Cleanup also runs on stdio EOF, `SIGINT`, or `SIGTERM`. A separate worker owns the desktop and runs cleanup if the client terminates the MCP launcher abruptly. Shutdown interrupts desktop startup and waits for any services already launched to exit before removing the runtime directory. A forced kill of the worker itself can leave processes and runtime files behind. App descendants that leave the managed process group can outlive the session.

## License

[MIT](LICENSE)
