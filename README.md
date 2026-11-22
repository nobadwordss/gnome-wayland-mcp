# GNOME Wayland MCP

Test native Linux GUI apps with an MCP agent on a separate GNOME Wayland display. The agent can launch a configured app, capture screenshots, click, and type while you keep using your regular desktop.

[Linux runtime requirements](#requirements). End-to-end testing covers Codex on Fedora Workstation 45 (x86_64) with Mutter 51.1. The release binary also passes the GTK smoke test on Ubuntu 26.04 LTS (x86_64) with Mutter 50.1. Claude Code and Gemini CLI have standard local stdio setup examples.

[Download for Linux x86_64](#download-the-linux-x86_64-binary) · [Codex](#codex) · [Claude Code](#claude-code) · [Gemini CLI](#gemini-cli)

![Codex opens GNOME Text Editor, clicks New Document, pastes a multiline checklist with an emoji, and finds a match with Ctrl+F](docs/gnome-text-editor-demo.gif)

*Actual v0.2.2 screenshots from the private display, shown alongside the MCP calls that produced them.* [Full-size screenshot from the original Codex test](docs/codex-headless-demo.png).

## Quick start

### Requirements

Use a Linux host with Mutter, D-Bus, PipeWire, WirePlumber 0.5 or later, and GStreamer installed under `/usr/bin`. The examples use GNOME Text Editor. On Fedora, install any missing runtime dependencies with the following command.

```sh
sudo dnf install mutter dbus-daemon pipewire wireplumber gstreamer1 \
  pipewire-gstreamer gstreamer1-plugins-base gstreamer1-plugins-good gnome-text-editor
```

On Ubuntu 26.04 LTS, install the distribution packages below, then [download the Linux x86_64 binary](#download-the-linux-x86_64-binary). Mutter and GNOME Text Editor are available in Ubuntu's Universe repository.

```sh
sudo apt update
sudo apt install curl mutter dbus-daemon pipewire wireplumber libgl1-mesa-dri \
  gstreamer1.0-tools gstreamer1.0-pipewire gstreamer1.0-plugins-base \
  gstreamer1.0-plugins-good gnome-text-editor
```

Ubuntu 24.04 LTS ships WirePlumber 0.4, below the required version.

### Download the Linux x86_64 binary

The [v0.2.4 release](https://github.com/nobadwordss/gnome-wayland-mcp/releases/tag/v0.2.4) provides a statically linked Linux x86_64 executable. You can install and run it without a Rust toolchain.

```sh
archive=gnome-wayland-mcp-v0.2.4-linux-x86_64.tar.gz
release=https://github.com/nobadwordss/gnome-wayland-mcp/releases/download/v0.2.4
curl -fLO "$release/$archive" &&
  curl -fLO "$release/$archive.sha256" &&
  sha256sum -c "$archive.sha256" &&
  tar -xzf "$archive" &&
  install -Dm755 "${archive%.tar.gz}/gnome-wayland-mcp" "$HOME/.local/bin/gnome-wayland-mcp"
```

### Build from source

Use Rust 1.91 or later to build the server from a checkout.

```sh
git clone https://github.com/nobadwordss/gnome-wayland-mcp.git
cd gnome-wayland-mcp
cargo build --locked --release
```

## Connect your MCP client

Use a client that launches local stdio servers and passes image tool results to the model. The examples assume the downloaded binary is in `~/.local/bin`. If you built from source, use the absolute path to `target/release/gnome-wayland-mcp`.

### Codex

```sh
codex mcp add gnome-wayland-mcp -- "$HOME/.local/bin/gnome-wayland-mcp" \
  -- /usr/bin/gnome-text-editor --standalone
codex mcp get gnome-wayland-mcp
```

### Claude Code

```sh
claude mcp add --transport stdio --scope user gnome-wayland-mcp \
  -- "$HOME/.local/bin/gnome-wayland-mcp" -- /usr/bin/gnome-text-editor --standalone
claude mcp get gnome-wayland-mcp
```

See the [Claude Code stdio setup guide](https://code.claude.com/docs/en/mcp#option-3-add-a-local-stdio-server).

### Gemini CLI

Add this entry under `mcpServers` in your user `~/.gemini/settings.json` and replace `/home/you` with your own absolute path.

```json
{
  "mcpServers": {
    "gnome-wayland-mcp": {
      "command": "/home/you/.local/bin/gnome-wayland-mcp",
      "args": ["--", "/usr/bin/gnome-text-editor", "--standalone"]
    }
  }
}
```

Run `gemini mcp list` to check the connection. See the [Gemini CLI MCP configuration guide](https://geminicli.com/docs/tools/mcp-server/).

## Try it

Open a new session in your MCP client and try this prompt with the GNOME Text Editor configuration above.

> Use the `gnome-wayland-mcp` tools to start the private desktop and launch GNOME Text Editor. Take a screenshot, click the editing area, and enter “Hello from the private desktop.” Take a new screenshot to verify the text, then stop the desktop.

To launch a different app, replace the executable and arguments after the server's `--` separator. Use an absolute path to a native Wayland executable. The default resolution is 1280 × 800. Pass `--width` and `--height` before the server's `--` to change it.

## Codex skill

Install the [`$gnome-wayland-mcp` skill](skills/gnome-wayland-mcp/SKILL.md) for guidance on running an app in the private desktop, capturing its screen, and verifying mouse and keyboard input. Follow the [Codex registration steps](#codex), then install the skill in your Codex user directory.

```sh
skill_file="$(mktemp)" &&
curl -fsSL https://raw.githubusercontent.com/nobadwordss/gnome-wayland-mcp/main/skills/gnome-wayland-mcp/SKILL.md \
  -o "$skill_file" && \
  install -Dm644 "$skill_file" "${CODEX_HOME:-$HOME/.codex}/skills/gnome-wayland-mcp/SKILL.md" &&
  rm -f "$skill_file"
```

Open a new Codex session and mention `$gnome-wayland-mcp` in your prompt.

### Firefox example

With native Wayland Firefox installed at `/usr/bin/firefox`, add a separate MCP entry for it. If you built the server from source, use the absolute path to `target/release/gnome-wayland-mcp` instead of the installed binary path.

```sh
codex mcp add gnome-wayland-firefox -- "$HOME/.local/bin/gnome-wayland-mcp" \
  -- /usr/bin/firefox --no-remote --new-instance about:blank
```

The server gives Firefox a temporary home, and the Firefox flags start a separate instance. Open a new Codex session and try the following prompt.

> Use `$gnome-wayland-mcp` with the `gnome-wayland-firefox` MCP entry. Start the private desktop and launch Firefox. Capture the window and click the address bar. Enter `https://example.com/`, then capture the window to verify the address. Press Enter, capture the loaded page, and stop the private desktop.

### Ptyxis example

Use this example with GNOME's Ptyxis terminal installed at `/usr/bin/ptyxis`, Bash at `/usr/bin/bash`, and `gnome-wayland-mcp` v0.2.2 or later registered in Codex. Launch Codex with an argument override for this invocation. The temporary `PATH` lets Ptyxis start Bash directly inside the private session by omitting the optional `systemd-run` launcher.

```sh
(
  set -eu
  test_path=$(mktemp -d /tmp/gnome-wayland-ptyxis.XXXXXXXX)
  trap 'rm -f -- "$test_path/bash"; rmdir -- "$test_path"' EXIT
  ln -s /usr/bin/bash "$test_path/bash"

  ptyxis_args=$(printf '%s' \
    '["--", "/usr/bin/env", "PATH='"$test_path"'", "LC_ALL=C.UTF-8", ' \
    '"PS1=paste-test $ ", "/usr/bin/ptyxis", "--standalone", ' \
    '"--", "/usr/bin/bash", "--noprofile", "--norc", "-i"]')
  codex -c "mcp_servers.gnome-wayland-mcp.args=$ptyxis_args"
)
```

In that Codex session, use `$gnome-wayland-mcp` and follow these steps.

1. Call `desktop_start`, then `launch_app`. Capture the terminal and click its pane once the shell prompt appears.
2. Call `type_text` with `{"text":"한글 붙여넣기 확인 🌱🙂","paste_shortcut":"ctrl_shift_v"}`. Send one line with no trailing newline. Leave the text at the prompt without pressing Enter.
3. Take a fresh screenshot and verify the Korean text and emoji on the same prompt line. If they have yet to appear, capture again before retrying.
4. Call `desktop_stop` and confirm that `desktop_status` reports `stopped`. Exit the test Codex invocation to remove the temporary `PATH` directory.

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
| Ptyxis | 50.1 |

The Codex test verified screenshots, coordinate clicks, shortcut keys, and Unicode input. The text editor saved the expected UTF-8 text, including a line break and an emoji.

The GTK smoke test passes on Ubuntu 26.04 LTS (x86_64) with Mutter 50.1, PipeWire 1.6.2, WirePlumber 0.5.13, and GStreamer 1.28.2. It covers capture, mouse and keyboard input, repeated Unicode paste, stale frame rejection, and session cleanup. To run it on your host, install Python with PyGObject, GTK 4, and Pillow. On Ubuntu, install `python3-gi gir1.2-gtk-4.0 python3-pil` and use `/usr/bin/python3`.

```sh
python3 -B tests/headless_smoke.py target/release/gnome-wayland-mcp
```

The clipboard integration tests run against a disposable D-Bus daemon and pass real Unix file descriptors. They exercise late transfer signals, missing transfers, and stalled clipboard writes. Run them with the Rust toolchain and `/usr/bin/dbus-daemon` installed.

```sh
cargo test --locked mutter::tests
```

The server uses Mutter's private D-Bus interfaces, which can change between versions. The table records the versions used for end-to-end testing.

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
| `type_text` | Pastes up to 16 KiB of UTF-8 from `{text}` into the focused editable field. Defaults to Ctrl+V. |
| `desktop_stop` | Stops the desktop and the running app. |

Screenshot coordinates start at the top-left pixel. Each pointer action uses the current `frame_id`. A new screenshot, app launch, or input invalidates earlier IDs. A `dispatched` response confirms delivery to Mutter. For `type_text`, Mutter also acknowledges a clipboard transfer and can request the text for its own clipboard cache. Use a fresh screenshot to verify that the app inserted the text before pressing Enter or replacing the clipboard with another `type_text` call.

Pass `{"text":"Hello","paste_shortcut":"ctrl_shift_v"}` for apps whose paste shortcut is Ctrl+Shift+V. Omit `paste_shortcut`, pass `null`, or pass `"ctrl_v"` to use Ctrl+V.

## Session behavior

The private desktop owns its Wayland socket, D-Bus, PipeWire, keyboard focus, and clipboard. Screenshots and input apply to apps launched inside that desktop. The app gets a temporary home and profile, which are removed when the session ends. Save any files you want to retain to an explicit persistent path. After a successful `type_text` call, the text remains available to apps on the private clipboard until another paste replaces it or the session stops. Mutter can also retain a cached copy after a paste error. Check the app before retrying an error that reports possible dispatch.

The runtime uses the installed Mutter executable directly. The private display and input are separate from your regular session. Apps run with the invoking user's OS permissions and can access files and the network as that user.

Each MCP server process keeps at most one private desktop. Repeated `desktop_start` calls reuse a healthy desktop. Separate server processes can each own a desktop, and an idle server keeps its desktop until it receives a stop or shutdown request.

Use `desktop_stop` to release the desktop while the MCP client is open, then check for `stopped` with `desktop_status`. The server stops its app and desktop services and removes the temporary runtime directory. Cleanup also runs on stdio EOF, `SIGINT`, or `SIGTERM`. A separate worker owns the desktop and runs cleanup if the client terminates the MCP launcher abruptly. Shutdown interrupts desktop startup and waits for any services already launched to exit before removing the runtime directory. A forced kill of the worker itself can leave processes and runtime files behind. App descendants that leave the managed process group can outlive the session.

## License

[MIT](LICENSE)
