"""Verify compound MCP pointer input on a private, rendered GTK text view."""

from itertools import groupby
from pathlib import Path
import sys
import tempfile
import time

from click_disconnect import hold_snapshot_eof, pointer_messages
from headless_smoke import (
    Client, child_executable, children, disconnect_capture_link, initialize, point, poll,
    snapshot, stopped_process,
)


def text_rows(pixels):
    # Locate the first two lines of ink against the text view's own background.
    rows = []
    for y in range(110, 186):
        background = pixels.getpixel((780, y))
        for x in range(452, 525):
            color = pixels.getpixel((x, y))
            if max(abs(a - b) for a, b in zip(color, background)) > 85:
                rows.append(y)
                break
    bands = [list(group) for _, group in groupby(enumerate(rows), lambda pair: pair[1] - pair[0])]
    return {"lines": [(band[0][1] + band[-1][1]) // 2 for band in bands]}


def exercise_selection(client, report):
    frame = poll(lambda: client.call("screenshot"), lambda f: f["pixel"] == (38, 64, 90), "GTK visible")
    client.call("mouse_click", {"point": point(frame, 700, 150), "count": 1})
    body = "bravo delta\nalpha bravo\ncharlie"
    client.call("type_text", {"text": body})
    poll(lambda: snapshot(report), lambda s: s["text"] == body, "GTK text inserted")
    frame = poll(lambda: client.call("screenshot", inspect_frame=text_rows),
                 lambda f: len(f["lines"]) >= 3, "rendered lines")
    # Place the caret at the far end of the rendered line. The following
    # double-click must start its own sequence even on a fast runner.
    caret = {"point": point(frame, 700, frame["lines"][0])}
    for count in (0, 3):
        client.call("mouse_click", {**caret, "count": count}, error="count must be 1 or 2")
    client.call("mouse_click", caret)
    frame = client.call("screenshot", inspect_frame=text_rows)
    click = {"point": point(frame, 460, frame["lines"][0])}
    assert client.call("mouse_click", {**click, "count": 2})["status"] == "dispatched"
    selection = poll(lambda: snapshot(report),
                     lambda s: s.get("selected") == "bravo"
                     and [p["count"] for p in s.get("editor_presses", [])[-2:]] == [1, 2],
                     "GTK recognized a double-click and selected bravo")
    client.call("mouse_click", click, error="stale or unknown")

    client.call("press_key", {"key": "End"})
    frame = client.call("screenshot", inspect_frame=text_rows)
    second = frame["lines"][1]
    drag = {"frame_id": frame["frame_id"], "start": {"x": 700, "y": second},
            "end": {"x": 451, "y": second}}
    client.call("mouse_drag", {**drag, "end": {"x": frame["width"], "y": second}}, error="outside")
    client.call("mouse_drag", {**drag, "end": drag["start"]}, error="endpoints must differ")
    assert client.call("mouse_drag", drag)["status"] == "dispatched"
    poll(lambda: snapshot(report), lambda s: s.get("selected") == "alpha bravo", "reverse drag selection")
    client.call("mouse_drag", drag, error="stale or unknown")
    print(f"GTK selected 'bravo' with presses {selection['editor_presses'][-3:]} "
          "and 'alpha bravo' with a reverse drag", flush=True)


def exercise_disconnect(client, report, app, *, drag):
    frame = client.call("screenshot")
    runtime = Path(app["runtime"])
    (worker,) = children(client.process.pid)
    monitor = child_executable(worker, "pw-dump")
    mutter = child_executable(worker, "mutter")
    if drag:
        tool = "mouse_drag"
        args = {"frame_id": frame["frame_id"], "start": {"x": 480, "y": 185},
                "end": {"x": 807, "y": 195}}
    else:
        tool = "mouse_click"
        args = {"point": point(frame, 480, 185), "count": 2}

    with pointer_messages(app["bus"]) as (wait_for, seen):
        client.write("tools/call", {"name": tool, "arguments": args})
        wait_for("boolean true")
        if drag:
            wait_for("member=NotifyPointerMotionAbsolute")  # A movement while the button is down.
            hold = stopped_process(mutter)
        else:
            wait_for("boolean false")  # First click finished. Hold the second pre-press check.
            hold = hold_snapshot_eof(worker, runtime)
        with hold:
            disconnect_capture_link(runtime)
            poll(lambda: monitor in children(worker), lambda alive: not alive, "MCP observed link loss")
        result = client.receive(tool)
        assert result.get("isError"), result
        notice = result["content"][0]["text"]
        assert "partially delivered" in notice and "Do not retry" in notice, notice
        fresh = client.call("screenshot")
        client.call(tool, args, error="stale or unknown")
        client.call("mouse_move", point(fresh, 602, 465))
        wait_for("double 465.5")  # Drain through the verification motion.
        buttons = [line for line in seen if line.startswith("boolean ")]
        assert buttons == ["boolean true", "boolean false"], seen
        after_release = seen[seen.index("boolean false") + 1:]
        assert sum("member=NotifyPointerMotionAbsolute" in line for line in after_release) == 1, seen
        if drag:
            assert "double 807.5" not in seen, seen  # Drag stopped before its requested endpoint.
    poll(lambda: snapshot(report),
         lambda s: abs(s["pointer"][0] - 602) <= 1 and abs(s["pointer"][1] - 465) <= 1
         and not s["left_down"], "GTK observed free pointer motion after recovery")
    print(f"Passed {tool} disconnect: one press, one release, GTK button up", flush=True)


def exercise_late_double_click(client, app):
    frame = client.call("screenshot")
    (worker,) = children(client.process.pid)
    args = {"point": point(frame, 480, 185), "count": 2}
    with pointer_messages(app["bus"]) as (wait_for, seen):
        client.write("tools/call", {"name": "mouse_click", "arguments": args})
        wait_for("boolean false")
        with hold_snapshot_eof(worker, Path(app["runtime"])):
            time.sleep(0.35)  # Delay the second pre-press check past the server's 200 ms budget.
        reply = client.receive("mouse_click")
        notice = reply["content"][0]["text"]
        assert reply.get("isError") and "interval elapsed" in notice and "Do not retry" in notice, reply
        client.call("mouse_move", point(client.call("screenshot"), 602, 465))
        wait_for("double 465.5")
        assert [line for line in seen if line.startswith("boolean ")] == ["boolean true", "boolean false"], seen
    print("Passed delayed double-click: late second press suppressed", flush=True)


def main():
    fixture = Path(__file__).with_name("gtk_fixture.py").resolve()
    with tempfile.TemporaryDirectory(prefix="gwm-gestures-") as directory:
        report = Path(directory) / "gtk.json"
        client = Client(str(Path(sys.argv[1]).resolve()),
                        ["/usr/bin/env", "GTK_A11Y=none", sys.executable, fixture, report], sys.stderr)
        try:
            initialize(client)
            client.call("desktop_start")
            client.call("launch_app")
            app = poll(lambda: snapshot(report), lambda s: s.get("ready"), "GTK window")
            exercise_selection(client, report)
            exercise_disconnect(client, report, app, drag=True)
            exercise_disconnect(client, report, app, drag=False)
            exercise_late_double_click(client, app)
        finally:
            client.close()


if __name__ == "__main__":
    main()
