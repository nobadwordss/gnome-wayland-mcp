"""A real GTK4 clipboard target and a visual marker for capture freshness."""
import json
import os
from pathlib import Path
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, GLib, Gtk


directory = Path(sys.argv[1])
report = directory / "app.json"
flip = directory / "flip-marker"
state = {"pid": os.getpid(), "ready": False, "text": "", "marker": "before",
         "session_runtime": os.environ["PIPEWIRE_RUNTIME_DIR"]}


def publish():
    pending = report.with_suffix(".tmp")
    pending.write_text(json.dumps(state), encoding="utf-8")
    pending.replace(report)


def activate(application):
    window = Gtk.ApplicationWindow(application=application, title="GTK clipboard benchmark")
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
    box.add_css_class("bench")
    stage = Gtk.Box()
    stage.set_size_request(-1, 90)
    box.append(stage)
    editor = Gtk.TextView()
    editor.set_margin_start(100)
    editor.set_margin_end(100)
    editor.set_size_request(-1, 180)
    box.append(editor)
    window.set_child(box)
    window.set_focus(editor)

    colors = Gtk.CssProvider()
    colors.load_from_string(".bench { background-color: #26405a; }")
    Gtk.StyleContext.add_provider_for_display(
        Gdk.Display.get_default(), colors, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )

    def record_text(buffer):
        state["text"] = buffer.get_text(buffer.get_start_iter(), buffer.get_end_iter(), False)
        publish()

    editor.get_buffer().connect("changed", record_text)
    window.fullscreen()
    window.present()
    editor.grab_focus()

    def tick():
        if not state["ready"] and window.get_width() == 960 and window.get_height() == 540:
            editor.grab_focus()
            state["ready"] = True
            publish()
        if flip.exists() and state["marker"] == "before":
            colors.load_from_string(".bench { background-color: #612d45; }")
            state["marker"] = "after"
            publish()
        return GLib.SOURCE_CONTINUE

    GLib.timeout_add(16, tick)


app = Gtk.Application(application_id="org.example.HeadlessClipboardBenchmark",
                      flags=0)
app.connect("activate", activate)
raise SystemExit(app.run([]))
