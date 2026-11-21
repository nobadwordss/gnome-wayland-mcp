# GNOME Wayland MCP

A local Rust MCP server that gives Codex a private headless GNOME Wayland desktop. It starts Mutter with one virtual monitor, launches a configured native Wayland app, and provides screenshots, mouse input, keyboard shortcuts, and Unicode paste. You can keep using your regular desktop while the agent works.

![GNOME Text Editor running on a private 1280 × 800 Wayland desktop after Codex clicked it and entered two lines of English text](docs/codex-headless-demo.png)

*Actual `screenshot` response from the registered MCP running through Codex. Codex launched GNOME Text Editor, clicked its editing area, and entered the text shown.*

## Quick start

Both installation methods require Codex and Linux with Mutter, D-Bus, PipeWire, WirePlumber 0.5 or later, and GStreamer installed under `/usr/bin`. The example uses GNOME Text Editor. On Fedora, install any missing runtime dependencies with the following command.

```sh
sudo dnf install mutter dbus-daemon pipewire wireplumber gstreamer1 \
  pipewire-gstreamer gstreamer1-plugins-base gstreamer1-plugins-good gnome-text-editor
```

### Download the Linux x86_64 binary

The [v0.1.0 release](https://github.com/nobadwordss/gnome-wayland-mcp/releases/tag/v0.1.0) provides a statically linked Linux x86_64 executable. You can install and run it without a Rust toolchain.

```sh
archive=gnome-wayland-mcp-v0.1.0-linux-x86_64.tar.gz
release=https://github.com/nobadwordss/gnome-wayland-mcp/releases/download/v0.1.0
curl -fLO "$release/$archive"
curl -fLO "$release/$archive.sha256"
sha256sum -c "$archive.sha256" && tar -xzf "$archive" && \
  install -Dm755 "${archive%.tar.gz}/gnome-wayland-mcp" "$HOME/.local/bin/gnome-wayland-mcp"
```

Register the installed executable with Codex.

```sh
codex mcp add gnome-wayland-mcp -- "$HOME/.local/bin/gnome-wayland-mcp" \
  -- /usr/bin/gnome-text-editor --standalone
codex mcp get gnome-wayland-mcp
```

### Build from source

Use Rust 1.91 or later to build and register the server from a checkout.

```sh
git clone https://github.com/nobadwordss/gnome-wayland-mcp.git
cd gnome-wayland-mcp
cargo build --locked --release
codex mcp add gnome-wayland-mcp -- "$PWD/target/release/gnome-wayland-mcp" \
  -- /usr/bin/gnome-text-editor --standalone
codex mcp get gnome-wayland-mcp
```

### Try it in Codex

Open a new Codex session and try the following prompt.

> Use `gnome-wayland-mcp` to start the private desktop and launch the configured app. Take a screenshot, click the editing area, and enter “Hello from the private desktop.” Take another screenshot to verify the text, then stop the desktop.

To choose a different app, change the executable and arguments after the second `--`. Pass an absolute path to a native Wayland executable. The default resolution is 1280 × 800. To change it, pass `--width` and `--height` before the second `--`.

## Verified environment

| Component | Tested version |
| --- | --- |
| OS | Fedora Workstation 45, x86_64 |
| GNOME Shell and Mutter | 51.1 |
| PipeWire | 1.6.11 |
| WirePlumber | 0.5.19 |
| GStreamer | 1.28.9 |
| Rust | 1.100.0 |
| Codex CLI | 0.171.0-alpha.13 |
| GNOME Text Editor | 51.0 |

The Codex test verified screenshots, coordinate clicks, shortcut keys, and Unicode input. The text editor saved the expected UTF-8 text, including a line break and an emoji. The included GTK smoke test also checks mouse movement, scrolling, stale frame rejection, and session cleanup. It needs Python with PyGObject, GTK 4, and Pillow:

```sh
python3 -B tests/headless_smoke.py target/release/gnome-wayland-mcp
```

This is an initial release. Compatibility beyond the environment above remains unverified. The server uses Mutter's private D-Bus interfaces, which can change between versions.

## MCP tools

| Tool | What it does |
| --- | --- |
| `desktop_start` | Starts a private Mutter desktop with one virtual monitor. |
| `desktop_status` | Reports desktop and application status. |
| `launch_app` | Starts the app specified in the server's arguments. |
| `screenshot` | Returns a PNG and its `frame_id`, `width`, and `height`. |
| `mouse_move` | Moves the pointer to `{frame_id, x, y}`. |
| `mouse_click` | Clicks `{point: {frame_id, x, y}, button}`. Defaults to `left`. |
| `mouse_scroll` | Scrolls at `{point: {frame_id, x, y}, vertical, horizontal}`. Positive steps move down or right. |
| `press_key` | Sends `{key, modifiers}`. Supports lowercase ASCII letters, digits, and named keys such as `Enter` and `Left`. |
| `type_text` | Pastes `{text}` into the focused editable field. Accepts up to 16 KiB of UTF-8. |
| `desktop_stop` | Stops the desktop and the running app. |

Screenshot coordinates start at the top-left pixel. Each pointer action uses the current `frame_id`. A new screenshot, app launch, or input invalidates earlier IDs. A `dispatched` response confirms delivery to Mutter. Use a fresh screenshot to confirm the app's response, especially before sending a key immediately after a paste.

## Session behavior

The private desktop owns its Wayland socket, D-Bus, PipeWire, keyboard focus, and clipboard. Screenshots and input apply to apps launched inside that desktop. The app gets a temporary home and profile, which are removed when the session ends. Save any files you want to retain to an explicit persistent path. The last text supplied to `type_text` remains available to apps on the private clipboard until another paste replaces it or the session stops.

The runtime uses the installed Mutter executable directly. Docker, GNOME Shell extensions, Xwayland, RDP, and host screen-sharing permission are optional to the host and unnecessary for this server. Apps run with the invoking user's OS permissions and can access files and the network as that user. Choose an app you trust.

The server stops the private session on `desktop_stop`, stdio closure, `SIGINT`, or `SIGTERM`. If the server is killed with `SIGKILL`, or an app moves into a separate process group, some processes may remain.

## License

[MIT](LICENSE)
