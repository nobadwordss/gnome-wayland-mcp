"""A real Wayland client for the opt-in MCP smoke test."""

import json
import os
from pathlib import Path
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, GLib, Gtk

report = Path(sys.argv[1])
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
    styles.load_from_string(".stage { background-color: #26405a; }")
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

    entry = Gtk.Entry(placeholder_text="Paste target")
    entry.set_size_request(400, 60)
    entry.connect("changed", lambda widget: update(text=widget.get_text()))
    stage.put(entry, 450, 110)

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
    window.present()

    def ready():
        if window.get_width() < 850 or window.get_height() < 445:
            return GLib.SOURCE_CONTINUE
        update(ready=True, width=window.get_width(), height=window.get_height())
        return GLib.SOURCE_REMOVE

    GLib.timeout_add(50, ready)


app = Gtk.Application(application_id="org.example.PrivateMcpSmokeTest")
app.connect("activate", activate)
raise SystemExit(app.run([]))
