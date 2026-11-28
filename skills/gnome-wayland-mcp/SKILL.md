---
name: gnome-wayland-mcp
description: Use the GNOME Wayland MCP to launch a configured app on a private headless desktop, capture it, and interact with it by mouse and keyboard. Use for desktop tasks and input tests inside that dedicated session.
---

# GNOME Wayland MCP

Use the registered `gnome-wayland-mcp` server for this workflow. It controls apps launched in its own headless Mutter session. Its display, pointer, keyboard focus, clipboard, and temporary app profile are separate from the user's desktop.

## Start and observe

1. Check `desktop_status` if a session may already be running. Use `desktop_start` to start the private desktop, then `launch_app` to start its configured app.
2. `launch_app` takes no arguments. The server runs the executable and arguments after its own `--` separator. If the app is unclear, inspect the registration with `codex mcp get gnome-wayland-mcp`. For a different app, select an entry configured for it or use a session-level argument override when available. Use a separate browser profile or a fresh instance for applications that can reuse an existing process.
3. Call `screenshot` and inspect the returned PNG. If the app is still starting, capture again before choosing a target. Use unscaled pixels from the returned image with the top-left corner as `(0, 0)`.

## Interact and verify

- Use `mouse_click` with `{"point":{"frame_id":"…","x":320,"y":140}}` to focus or activate a visible target. `mouse_move` takes the point directly. `mouse_scroll` takes `point` plus `vertical` or `horizontal` wheel steps. Each pointer action needs the latest `frame_id`. A new capture, app launch, or input invalidates earlier IDs.
- When the click schema offers `count`, use `count: 2` for a double-click. When `mouse_drag` is available, pass `{"frame_id":"…","start":{"x":320,"y":140},"end":{"x":480,"y":140}}` using two distinct pixels from the same screenshot. Verify the selection or drop with a fresh screenshot. A partial-delivery error calls for inspecting the app before taking further action.
- Focus an editable field before calling `type_text` with `{"text":"Hello"}`. This pastes UTF-8 using the private clipboard. When the `type_text` schema offers `paste_shortcut` and the app uses Ctrl+Shift+V, pass `"paste_shortcut":"ctrl_shift_v"`. Use `press_key` for actual key events and shortcuts, for example `{"key":"s","modifiers":["ctrl"]}` or `{"key":"Enter"}`.
- A `dispatched` response confirms delivery to Mutter. After a paste, capture again and verify that the app inserted the text before pressing Enter or sending another `type_text` call. If a frame is stale, capture again and choose the point from the new image. If an input may have been partially delivered, inspect the current app state before retrying.

## Ptyxis example

Use the [Ptyxis launch command](https://github.com/nobadwordss/gnome-wayland-mcp#ptyxis-example) to open a Codex invocation configured for Ptyxis. It uses a temporary `PATH` and starts Bash directly in the private session. Start the desktop, launch Ptyxis, and click the terminal pane after the prompt appears.

Call `type_text` with `{"text":"한글 붙여넣기 확인 🌱🙂","paste_shortcut":"ctrl_shift_v"}`. Keep the sample on one line with no trailing newline. Capture the pending text at the shell prompt without pressing Enter.

## Finish

Save any requested files to an explicit persistent path. Stop one-off test sessions with `desktop_stop` after capturing the result, then confirm `stopped` with `desktop_status` before exiting Codex. The private app home disappears when its session ends. Each server process reuses its healthy desktop across repeated `desktop_start` calls.
