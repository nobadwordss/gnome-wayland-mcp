# Verification

## Ubuntu and Fedora GTK regressions

For release verification, run the GTK, click-delivery, and pointer gesture tests against the same executable on Ubuntu 26.04 and Fedora Workstation. The Ubuntu CI job also runs all three tests on a source build. Run the commands below from the repository root. [Ubuntu CI workflow](../.github/workflows/ubuntu-smoke.yml)

The v0.2.7 Linux x86_64 release executable passed all three tests on Fedora Workstation 45 (Mutter 51.1, PipeWire 1.6.12) and Ubuntu 26.04.1 LTS (Mutter 50.1, PipeWire 1.6.2).

`headless_smoke.py` checks the desktop, application and capture status while a session is running, paused or interrupted. It removes the private PipeWire link, immediately sends a click with the previous frame ID, and checks the first recovery PNG and GTK application PID. It repeats the check with the old capture process held open. `click_disconnect.py` disconnects after the last pre-press graph snapshot or after the button press appears on D-Bus. It checks the MCP response, D-Bus events, GTK button counters and the first recovered PNG.

`pointer_gestures.py` places the caret at the far end of `bravo delta`, takes a fresh screenshot, then immediately double-clicks `bravo`. It checks that the text view's own GTK recognizer counted the presses as 1 and 2 and that exactly `bravo` was selected. On failure the fixture reports recent press counts, coordinates, event times and the selected text. The test also checks right-to-left drag selection of `alpha bravo`, button release after a capture interruption and suppression of a late second click.

Install Python with PyGObject, GTK 4, Pillow, the PipeWire utilities and `dbus-monitor`. On Ubuntu, install `python3-gi gir1.2-gtk-4.0 python3-pil pipewire-bin dbus-bin` and use `/usr/bin/python3`. On Fedora, the PipeWire utilities are in `pipewire-utils`.

```sh
python3 -B tests/headless_smoke.py target/release/gnome-wayland-mcp
python3 -B tests/click_disconnect.py target/release/gnome-wayland-mcp
python3 -B tests/pointer_gestures.py target/release/gnome-wayland-mcp
```

The clipboard integration tests run against a disposable D-Bus daemon and pass real Unix file descriptors. They exercise late transfer signals, missing transfers, and stalled clipboard writes. Run them with the Rust toolchain and `/usr/bin/dbus-daemon` installed.

```sh
cargo test --locked mutter::tests
```

To check installer changes, run `python3 -B tests/install_codex.py` with Codex CLI on your `PATH` and access to the GitHub release assets. It verifies the default app choice, custom arguments, desktop-only setup, and preservation of an existing entry using temporary user configurations.

## Tested environments

The server uses Mutter's private D-Bus interfaces. The following versions were used for end-to-end tests.

| Component | Tested version |
| --- | --- |
| OS | Fedora Workstation 45, x86_64 |
| GNOME Shell and Mutter | 51.1 |
| PipeWire | 1.6.11, 1.6.12 |
| WirePlumber | 0.5.19 |
| GStreamer | 1.28.9 |
| Rust | 1.100.0 |
| Codex CLI | 0.171.0-alpha.13 |
| GNOME Text Editor | 51.0 |
| Ptyxis | 50.1 |

The Codex test verified screenshots, coordinate clicks, shortcut keys, and Unicode input. The text editor saved the expected UTF-8 text, including a line break and an emoji.

The v0.2.6 release passed its GTK smoke test on Ubuntu 26.04 LTS (x86_64) with Mutter 50.1, PipeWire 1.6.2, WirePlumber 0.5.13, and GStreamer 1.28.2. It covered capture, mouse and keyboard input, repeated Unicode paste, stale frame rejection, and session cleanup.

## Codex automatic selection smoke test

With the runtime and GTK smoke test dependencies installed, sign in to Codex CLI and run the following from this checkout. On Fedora, the fixture uses Noto Sans CJK KR from `google-noto-sans-cjk-vf-fonts` to display Korean syllables. You can also pass the verified v0.2.5 release executable as the first argument.

```sh
python3 -B tests/codex_routing_smoke.py target/release/gnome-wayland-mcp \
  --model gpt-6.2-sol --reasoning-effort high
```

The script starts two fresh Codex sessions and supplies only this local MCP server. The GTK fixture keeps its window hidden through the first screenshot after launch. The GUI request passes when Codex captures the visible window before input, pastes the exact two Korean lines, takes a new screenshot, reports the code shown after input, and stops the desktop with a `stopped` status. The file-only request must return the second line of `sample.txt` with zero MCP calls. The printed artifact directory contains the prompts, JSONL events, and PNG evidence.

The script stages the bundled skill in each temporary workspace by default. Add `--without-skill` to test tool selection with that skill disabled. It applies per-tool approval overrides for click, paste, key, and stop on the disposable server for each test invocation. Model, CLI version, and available tools can affect selection.

On Fedora Workstation 45 with Mutter 51.1, v0.2.5 passed both requests in one run with the skill enabled and three runs with it disabled. These runs used Codex CLI 0.171.0-alpha.13 and `gpt-6.2-sol` with high reasoning effort.

## wbox comparison on Fedora

This benchmark uses GNOME Wayland MCP v0.2.5 and wbox-mcp 0.15.0 on the same Fedora Workstation 45 x86_64 host with Mutter 51.1, GTK 4.24.1, and GStreamer 1.28.9. Both use headless Mutter, a 960×540 output, and the same GTK4 text field. The two wbox settings differ only in `timeouts.app_render`, which defaults to 3 seconds. [GNOME Wayland MCP v0.2.5](https://github.com/nobadwordss/gnome-wayland-mcp/releases/tag/v0.2.5) · [wbox 0.15.0](https://github.com/quazardous/wbox-mcp/tree/0de008ed922ebf9dc05c213fb047d6952d015474) · [wbox startup wait](https://github.com/quazardous/wbox-mcp/blob/0de008ed922ebf9dc05c213fb047d6952d015474/src/wbox/compositor/mutter.py#L246-L268)

The runner starts a fresh MCP process and desktop each time. It runs each condition 12 times, using all six orders twice. Every condition uses the same GTK readiness check and first-PNG color marker. The runner then checks the exact Korean text in the GTK buffer, pauses the session's GStreamer capture process for 6 seconds, and expects an error during the pause. After resuming it checks for the new PNG marker and the same app PID and text. The run ends when the app, capture, desktop, and server have exited and the private runtime has been removed. GNOME Wayland MCP uses `type_text`. Both wbox settings use `clipboard_write` followed by `hold` for Ctrl+V. [Benchmark runner](../benchmarks/wbox_mutter.py)

| Metric | GNOME Wayland MCP v0.2.5 | wbox default (3 s) | wbox `app_render: 0` |
| --- | ---: | ---: | ---: |
| Failed trials | 0/12 | 0/12 | 0/12 |
| First complete success | 8.29 s | 11.28 s | 8.92 s |
| Complete run, median (min–max) | 8.23 s (8.16–8.29) | 11.22 s (11.10–11.32) | 8.92 s (8.84–9.14) |
| MCP start to app launch reply, median | 1.20 s | 4.03 s | 0.95 s |
| Launch reply to first verified app frame, median | 0.59 s | 0.23 s | 0.99 s |
| MCP start to verified Korean paste, median | 1.81 s | 4.79 s | 2.46 s |
| Capture error during the pause, median | 0.2 ms | 5.40 s | 5.40 s |
| Resume to verified new PNG, median | 0.30 s | 0.26 s | 0.26 s |

Complete times run from MCP process start to cleanup and include the 6-second pause. First complete success counts only elapsed trials for that condition through its first pass. All three passed on their first trial. Medians and ranges use successful trials. The expected error during the pause counts toward a pass.

Setting the wbox wait to zero reduced the median complete run by 2.30 seconds. The median wait from the launch reply to the first verified app frame rose from 0.23 to 0.99 seconds. With the fixed wait disabled, the median complete run was 0.69 seconds longer than GNOME Wayland MCP in this benchmark. These measurements cover one GTK workload with an explicit readiness check on the wbox Mutter backend.

For reproduction, install the [runtime dependencies](../README.md#requirements) and the Fedora packages `python3-gobject`, `python3-pillow`, `gtk4`, `pipewire-pulseaudio`, and `wl-clipboard`. Download and verify the v0.2.5 release archive using its accompanying SHA-256 file. With `uv` installed, run these commands from this repository.

```sh
bench_deps=$(mktemp -d)
git clone https://github.com/quazardous/wbox-mcp.git "$bench_deps/wbox-mcp"
git -C "$bench_deps/wbox-mcp" checkout --detach 0de008ed922ebf9dc05c213fb047d6952d015474
uv sync --project "$bench_deps/wbox-mcp" --frozen --python 3.14.10
uv pip install --python "$bench_deps/wbox-mcp/.venv/bin/python" 'jeepney==0.9.0'
/usr/bin/python3 -B benchmarks/wbox_mutter.py \
  /path/to/v0.2.5/gnome-wayland-mcp "$bench_deps/wbox-mcp/.venv/bin/wbox-mcp" \
  --output "$bench_deps/results"
```

The runner writes `trials.jsonl`, server logs, and before/after PNGs to the new output directory. It records the stage and reason for each failed trial and writes `summary.json` at the end. If a trial fails, the exit status is nonzero. The runner stops early if it cannot verify session cleanup.

## Native Wayland Electron smoke test

The opt-in smoke test passed with Electron 45.4.1 on Fedora Workstation 45 and Ubuntu 26.04.1 LTS. It launches a decorated window with `--ozone-platform=wayland`, clicks positions from its screenshot, verifies focus and repeated Unicode paste, and checks process cleanup. It uses Electron's default sandbox and GPU settings.

Install Python with Pillow and extract the Electron Linux x64 archive. Run the test with the paths to the MCP server and the Electron executable. [Electron 45.4.1 downloads](https://github.com/electron/electron/releases/tag/v45.4.1)

```sh
python3 -B tests/electron_smoke.py target/release/gnome-wayland-mcp /path/to/electron
```

The Ubuntu VM used the archive's Chromium sandbox helper with Ubuntu's default AppArmor policy. Extracted binaries can require sandbox setup on hosts that restrict unprivileged user namespaces. [Chromium's AppArmor documentation](https://chromium.googlesource.com/chromium/src/+/main/docs/security/apparmor-userns-restrictions.md)
