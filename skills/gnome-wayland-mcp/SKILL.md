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
- Focus an editable field before calling `type_text` with `{"text":"Hello"}`. This pastes UTF-8 using the private clipboard. Use `press_key` for actual key events and shortcuts, for example `{"key":"s","modifiers":["ctrl"]}` or `{"key":"Enter"}`.
- A `dispatched` response confirms delivery to Mutter. Capture again to verify the app's response before sending a dependent action, especially before submitting immediately after a paste. If a frame is stale, capture again and choose the point from the new image. If an input may have been partially delivered, inspect the current app state before retrying.

## Finish

Save any requested files to an explicit persistent path. The private app home disappears when its session ends. Use `desktop_stop` to clean up a disposable test or when the user is done with the private desktop.
