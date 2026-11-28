"""Order a real PipeWire disconnect on either side of a private Mutter button press."""

from contextlib import ExitStack, contextmanager
import json
import os
from pathlib import Path
from queue import Queue
import select
import signal
import subprocess
import sys
import tempfile
from threading import Thread
import time

from headless_smoke import (
    Client, children, disconnect_capture_link, initialize, point, poll, snapshot, stopped_process,
)


@contextmanager
def pointer_messages(address):
    rule = "type='method_call',interface='org.gnome.Mutter.RemoteDesktop.Session',member="
    monitor = subprocess.Popen(
        ["/usr/bin/dbus-monitor", "--address", address,
         rule + "'NotifyPointerMotionAbsolute'", rule + "'NotifyPointerButton'"],
        stdout=subprocess.PIPE, text=True,
    )
    output = Queue()

    def read():
        for line in monitor.stdout:
            output.put(line.strip())
        output.put(None)

    reader = Thread(target=read, daemon=True)
    reader.start()
    seen = []

    def wait_for(fragment):
        deadline = time.monotonic() + 3
        while True:
            line = output.get(timeout=max(0.01, deadline - time.monotonic()))
            assert line is not None, ("D-Bus monitor exited", seen)
            seen.append(line)
            if fragment in line:
                return

    try:
        wait_for("member=NameLost")  # D-Bus has installed the monitor.
        yield wait_for, seen
    finally:
        monitor.terminate()
        monitor.wait(timeout=3)
        reader.join(timeout=3)
        monitor.stdout.close()


@contextmanager
def hold_snapshot_eof(worker, runtime):
    # The pointer has moved. The next short-lived pw-dump is the final pre-press
    # query. Hold a duplicate of its stdout writer, without reading or changing
    # any bytes. The query exits with the link still live, but MCP awaits EOF.
    deadline = time.monotonic() + 1.5
    with ExitStack() as files:
        while time.monotonic() < deadline:
            for pid in children(worker):
                proc = Path(f"/proc/{pid}")
                try:
                    argv = (proc / "cmdline").read_bytes().split(b"\0")[:-1]
                    if argv[:4] != [b"/usr/bin/pw-dump", b"-N", b"-r", os.fsencode(runtime / "pipewire-0")]:
                        continue
                    if not argv[-1].isdigit() or b"--monitor" in argv:
                        continue
                    handle = os.pidfd_open(pid)
                    files.callback(os.close, handle)
                    fd = os.open(proc / "fd/1", os.O_WRONLY | os.O_NONBLOCK)
                    files.callback(os.close, fd)
                except (FileNotFoundError, ProcessLookupError):
                    continue
                exited = select.poll()
                exited.register(handle, select.POLLIN)
                assert exited.poll(1000), "final pw-dump did not finish"
                yield
                return
            time.sleep(0.0002)
    raise AssertionError("Did not observe the final pre-press pw-dump")


def exercise(client, report, app, *, after_send):
    runtime = Path(app["runtime"])
    (worker,) = children(client.process.pid)
    processes = {Path(f"/proc/{pid}/exe").readlink().name: pid for pid in children(worker)}
    mutter = processes["mutter"]
    frame = client.call("screenshot")
    before = snapshot(report)
    was_replaced = before.get("target_replaced", False)
    colors = [(38, 64, 90), (97, 45, 69)]
    assert frame["pixel"] == colors[was_replaced]
    args = {"point": point(frame, 170, 135)}

    def unlink():
        disconnect_capture_link(runtime)
        # The MCP monitor reaps its pw-dump child after latching link loss.
        poll(lambda: processes["pw-dump"] in children(worker), lambda alive: not alive,
             "MCP observed the unlink")

    def change_target():
        os.kill(app["pid"], signal.SIGUSR1)
        poll(lambda: snapshot(report), lambda s: s.get("target_replaced") == (not was_replaced),
             "GTK changed the button at the old coordinates")

    def click_counts(s):
        return [s["clicks"], s.get("replacement_clicks", 0)]

    expected = click_counts(before)
    expected[was_replaced] += int(after_send)
    with pointer_messages(app["bus"]) as (wait_for, seen):
        with stopped_process(mutter):
            client.write("tools/call", {"name": "mouse_click", "arguments": args})
            wait_for("member=NotifyPointerMotionAbsolute")
        with ExitStack() as paused:
            with hold_snapshot_eof(worker, runtime):
                if after_send:
                    paused.enter_context(stopped_process(mutter))
                else:
                    unlink()
                    change_target()
            if after_send:
                wait_for("boolean true")  # The press is already on the bus while Mutter is held.
                unlink()
        reply = client.receive("mouse_click")
        assert reply.get("isError"), reply
        notice = reply["content"][0]["text"]
        if after_send:
            assert "click may have been delivered" in notice and "Do not retry" in notice, notice
            wait_for("boolean false")
            # Let GTK handle this one real click, then change the screen while
            # capture is still disconnected and before requesting a screenshot.
            poll(lambda: snapshot(report), lambda s: click_counts(s) == expected, "one GTK click")
            change_target()
        else:
            assert "Click was not sent" in notice and "interrupted" in notice, notice

        recovered = client.call("screenshot")  # Exactly the first capture request after the unlink.
        assert recovered["pixel"] == colors[not was_replaced] and recovered["frame_id"] != frame["frame_id"]
        client.call("mouse_click", args, error="stale or unknown")
        # A subsequent motion on the same connection fences the bus trace without adding a click.
        client.call("mouse_move", point(recovered, 600, 450))
        wait_for("member=NotifyPointerMotionAbsolute")
        buttons = [line for line in seen if line.startswith("boolean ")]
        assert buttons == (["boolean true", "boolean false"] if after_send else []), seen

    state = poll(lambda: snapshot(report), lambda s: click_counts(s) == expected, "GTK button counts")
    assert state["pid"] == app["pid"] and state["text"] == before["text"] == "preserved"
    assert {app["pid"], mutter} <= set(children(worker))
    assert processes["gst-launch-1.0"] not in children(worker)
    print(json.dumps({"timing": "after_send" if after_send else "before_send", "notice": notice,
                      "button_events": buttons, "gtk_before": click_counts(before), "gtk_after": expected,
                      "app_pid_before": app["pid"], "app_pid_after": state["pid"], "entry": state["text"],
                      "screenshot_pixel_before": frame["pixel"], "first_recovered_pixel": recovered["pixel"],
                      "old_frame_rejected": True}), flush=True)


def main():
    fixture = Path(__file__).with_name("gtk_fixture.py").resolve()
    with tempfile.TemporaryDirectory(prefix="gwm-click-test-") as directory:
        report = Path(directory) / "gtk.json"
        client = Client(str(Path(sys.argv[1]).resolve()), [sys.executable, fixture, report], sys.stderr)
        try:
            initialize(client)
            client.call("desktop_start")
            client.call("launch_app")
            app = poll(lambda: snapshot(report), lambda s: s.get("ready"), "GTK window")
            frame = poll(lambda: client.call("screenshot"), lambda f: f["pixel"] == (38, 64, 90), "GTK pixels")
            client.call("mouse_click", {"point": point(frame, 490, 138)})
            client.call("type_text", {"text": "preserved"})
            poll(lambda: snapshot(report), lambda s: s["text"] == "preserved", "GTK entry")
            exercise(client, report, app, after_send=False)
            exercise(client, report, app, after_send=True)
        finally:
            client.close()


if __name__ == "__main__":
    main()
