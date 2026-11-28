"""A real Wayland client for the opt-in MCP smoke test."""

import json
import os
from pathlib import Path
import signal
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, GLib, Gtk

report = Path(sys.argv[1])
window_gate = Path(sys.argv[2]) if len(sys.argv) > 2 else None
state = {
    "pid": os.getpid(),
    "runtime": os.environ["XDG_RUNTIME_DIR"],
    "wayland": os.environ["WAYLAND_DISPLAY"],
    "bus": os.environ["DBUS_SESSION_BUS_ADDRESS"],
    "pipewire_runtime": os.environ["PIPEWIRE_RUNTIME_DIR"],
    "x11": os.environ.get("DISPLAY"),
    "wayland_socket": os.environ.get("WAYLAND_SOCKET"),
    "clicks": 0,
    "motion": 0,
    "scroll": 0,
    "ctrl_pastes": 0,
    "shift_pastes": 0,
    "text": "",
}


def update(**values):
    state.update(values)
    temporary = report.with_suffix(".tmp")
    temporary.write_text(json.dumps(state), encoding="utf-8")
    temporary.replace(report)


def activate(app):
    window = Gtk.ApplicationWindow(application=app, title="Private MCP smoke test")
    stage = Gtk.Fixed()
    stage.add_css_class("stage")
    styles = Gtk.CssProvider()
    styles.load_from_string(
        ".stage { background-color: #26405a; }"
        ".stage.replaced { background-color: #612d45; }"
        ".screen-code { color: white; font-size: 22px; }"
        '.routing-editor { font-family: "Noto Sans CJK KR", sans-serif; }'
    )
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), styles, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )

    button = Gtk.Button(label="Click target")
    button.set_size_request(180, 70)
    button.connect("clicked", lambda _: update(clicks=state["clicks"] + 1))
    motion = Gtk.EventControllerMotion()
    motion.connect("enter", lambda *_: update(motion=state["motion"] + 1))
    button.add_controller(motion)
    stage.put(button, 100, 100)

    replacement = Gtk.Button(label="Replacement target")
    replacement.set_size_request(180, 70)
    replacement.connect("clicked", lambda _: update(replacement_clicks=state.get("replacement_clicks", 0) + 1))

    def switch_target():
        replaced = not state.get("target_replaced", False)
        if replaced:
            stage.add_css_class("replaced")
            stage.remove(button)
            stage.put(replacement, 100, 100)
        else:
            stage.remove_css_class("replaced")
            stage.remove(replacement)
            stage.put(button, 100, 100)
        update(target_replaced=replaced)
        return GLib.SOURCE_CONTINUE

    def replace_on_key(_controller, keyval, _keycode, _modifiers):
        if keyval != Gdk.KEY_F7:
            return False
        switch_target()
        return True

    window_keys = Gtk.EventControllerKey()
    window_keys.connect("key-pressed", replace_on_key)
    window.add_controller(window_keys)
    # Allow the test to change the screen without consuming its MCP frame ID.
    GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGUSR1, switch_target)

    editor = Gtk.TextView()
    editor.set_size_request(400, 100)
    if window_gate is not None:
        editor.add_css_class("routing-editor")
    buffer = editor.get_buffer()
    screen_code = None
    code_label = None

    def record_edit(widget):
        text = widget.get_text(widget.get_start_iter(), widget.get_end_iter(), False)
        if code_label is not None:
            lines = text.split("\n")
            suffix = f" · Screen code: {screen_code}" if len(lines) == 2 and all(lines) else ""
            code_label.set_text(f"GTK Text Editor{suffix}")
        update(text=text)

    buffer.connect("changed", record_edit)
    defer_paste = False
    pending_text = None

    def receive_paste(clipboard, result):
        nonlocal pending_text
        text = clipboard.read_text_finish(result)
        if defer_paste:
            pending_text = text
        elif text is not None:
            buffer.insert_at_cursor(text)

    def key_pressed(_controller, keyval, _keycode, modifiers):
        nonlocal defer_paste, pending_text
        if keyval == Gdk.KEY_F6:
            # Model an app that reads the clipboard asynchronously and commits it later.
            defer_paste = True
            update(paste_deferred=True)
            return True
        if keyval == Gdk.KEY_Return and defer_paste:
            defer_paste = False
            update(text_at_enter=state["text"])
            if pending_text is not None:
                buffer.insert_at_cursor(pending_text)
                pending_text = None
            return True
        if keyval not in (Gdk.KEY_v, Gdk.KEY_V):
            return False
        modifiers &= Gtk.accelerator_get_default_mod_mask()
        ctrl = Gdk.ModifierType.CONTROL_MASK
        if modifiers == ctrl:
            update(ctrl_pastes=state["ctrl_pastes"] + 1)
        elif modifiers == ctrl | Gdk.ModifierType.SHIFT_MASK:
            update(shift_pastes=state["shift_pastes"] + 1)
            editor.get_clipboard().read_text_async(None, receive_paste)
            return True
        return False

    keys = Gtk.EventControllerKey()
    keys.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
    keys.connect("key-pressed", key_pressed)
    editor.add_controller(keys)
    stage.put(editor, 450, 110)

    scroller = Gtk.ScrolledWindow()
    scroller.set_size_request(320, 225)
    rows = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
    for index in range(80):
        label = Gtk.Label(label=f"Scroll row {index + 1}")
        label.set_size_request(-1, 35)
        rows.append(label)
    scroller.set_child(rows)
    scroller.get_vadjustment().connect("value-changed", lambda adj: update(scroll=adj.get_value()))
    stage.put(scroller, 100, 220)

    window.set_child(stage)
    window.fullscreen()

    def ready():
        if window.get_width() < 850 or window.get_height() < 445:
            return GLib.SOURCE_CONTINUE
        update(ready=True, width=window.get_width(), height=window.get_height())
        return GLib.SOURCE_REMOVE

    def show_window():
        nonlocal screen_code, code_label
        if window_gate is not None:
            try:
                screen_code = window_gate.read_text(encoding="utf-8").strip()
            except FileNotFoundError:
                return GLib.SOURCE_CONTINUE
            code_label = Gtk.Label(label="GTK Text Editor")
            code_label.add_css_class("screen-code")
            stage.put(code_label, 450, 65)
        window.present()
        GLib.timeout_add(50, ready)
        return GLib.SOURCE_REMOVE

    if window_gate is None:
        show_window()
    else:
        GLib.timeout_add(50, show_window)


app = Gtk.Application(application_id="org.example.PrivateMcpSmokeTest")
app.connect("activate", activate)
raise SystemExit(app.run([]))
