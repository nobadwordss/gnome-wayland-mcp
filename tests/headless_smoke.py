"""Exercise the real MCP transport, a private Mutter desktop and a GTK client."""

import base64
from contextlib import contextmanager
import io
import json
import os
from pathlib import Path
from queue import Queue, Empty
import select
import shutil
import signal
import subprocess
import sys
import tempfile
from threading import Thread
import time

from PIL import Image


class Client:
    def __init__(self, binary, application, log, *, own_process_group=False):
        self.process = subprocess.Popen(
            [binary, "--width", "960", "--height", "540", "--", *application],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=log,
            start_new_session=own_process_group,
            env={**os.environ, "WAYLAND_DISPLAY": "wrong-host-wayland", "WAYLAND_SOCKET": "99999",
                 "DISPLAY": ":999", "DBUS_SESSION_BUS_ADDRESS": "unix:path=/nonexistent-host-bus",
                 "PIPEWIRE_RUNTIME_DIR": "/nonexistent-host-pipewire"},
        )
        self.lines = Queue()
        self.request_id = 0
        self.reader = Thread(target=self.read, daemon=True)
        self.reader.start()

    def read(self):
        for line in self.process.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def write(self, method, params=None, notification=False):
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        if not notification:
            self.request_id += 1
            message["id"] = self.request_id
        self.process.stdin.write(json.dumps(message).encode() + b"\n")
        self.process.stdin.flush()

    def send(self, method, params=None, notification=False):
        self.write(method, params, notification)
        if notification:
            return None
        return self.receive(method)

    def receive(self, method):
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            try:
                line = self.lines.get(timeout=max(0.01, deadline - time.monotonic()))
            except Empty as error:
                raise AssertionError(f"Timed out waiting for {method}") from error
            assert line is not None, f"MCP stopped while handling {method}"
            reply = json.loads(line)
            if reply.get("id") == self.request_id:
                assert "error" not in reply, reply.get("error")
                return reply["result"]
        raise AssertionError(f"Timed out waiting for {method}")

    def call(self, tool, args=None, error=None, *, inspect_frame=None):
        result = self.send("tools/call", {"name": tool, "arguments": args or {}})
        text = result["content"][0]["text"]
        if error is not None:
            assert result.get("isError") is True and error in text, (tool, text)
            return None
        assert not result.get("isError"), (tool, text)
        output = json.loads(text)
        if tool == "screenshot":
            image = result["content"][1]
            assert image["type"] == "image" and image["mimeType"] == "image/png"
            with Image.open(io.BytesIO(base64.b64decode(image["data"], validate=True))) as png:
                png.load()
                assert png.format == "PNG" and png.size == (960, 540)
                pixels = png.convert("RGB")
                output["pixel"] = pixels.getpixel((30, 30))
                if inspect_frame is not None:
                    output.update(inspect_frame(pixels))
        return output

    def close(self):
        if self.process.stdin and not self.process.stdin.closed:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=25)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=3)
            raise
        finally:
            self.reader.join(timeout=3)
            self.process.stdout.close()


def poll(read, condition, description, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = read()
        if condition(value):
            return value
        time.sleep(0.05)
    raise AssertionError(f"Timed out checking {description}, last result {value!r}")


def snapshot(report):
    try:
        return json.loads(report.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}


def point(frame, x, y):
    return {"frame_id": frame["frame_id"], "x": x, "y": y}


def initialize(client):
    init = client.send("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                      "clientInfo": {"name": "headless-smoke", "version": "1"}})
    assert init["serverInfo"]["name"] == "gnome-wayland-mcp"
    client.send("notifications/initialized", notification=True)


def assert_running_app(client, capture):
    status = client.call("desktop_status")
    assert status["status"] == "application_running" and status["application"] == {"status": "running"}, status
    assert status["capture"] == {"status": capture}, status


def exercise_application_status(binary, root, log):
    unconfigured = Client(binary, [], log)
    try:
        initialize(unconfigured)
        status = unconfigured.call("desktop_status")
        assert status["status"] == "stopped" and not status["application_configured"]
        assert status["application"] == {"status": "not_configured"}
        assert status["capture"] == {"status": "stopped"}
        assert unconfigured.call("desktop_start")["status"] == "desktop_running"
        assert unconfigured.call("screenshot")["frame_id"]
        unconfigured.call("launch_app", error="configure an application")
        assert unconfigured.call("desktop_stop")["status"] == "stopped"
    finally:
        unconfigured.close()

    request = root / "app-exit"
    program = """\
from pathlib import Path
import signal
import sys
import time

request = Path(sys.argv[1])
while not request.exists():
    time.sleep(0.01)
reason = request.read_text(encoding="utf-8")
if reason == "term":
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    signal.raise_signal(signal.SIGTERM)
sys.exit(int(reason))
"""
    client = Client(binary, [sys.executable, "-c", program, request], log)
    try:
        initialize(client)
        not_started = {"status": "not_started"}
        initial = client.call("desktop_status")
        assert initial["status"] == "stopped" and initial["application"] == not_started
        assert initial["capture"] == {"status": "stopped"}
        desktop = client.call("desktop_start")
        assert desktop["status"] == "desktop_running" and "application" not in desktop and "capture" not in desktop
        ready = {"status": "ready"}
        assert client.call("desktop_status") == {**desktop, "application": not_started, "capture": ready}

        for reason, termination in (
            ("23", {"exit_code": 23}),
            ("0", {"exit_code": 0}),
            ("term", {"signal": signal.SIGTERM}),
        ):
            request.unlink(missing_ok=True)
            running = {**desktop, "status": "application_running"}
            assert client.call("launch_app") == running
            assert_running_app(client, "ready")

            pending = request.with_suffix(".tmp")
            pending.write_text(reason, encoding="utf-8")
            pending.replace(request)
            exited = poll(lambda: client.call("desktop_status"),
                          lambda state: state["application"]["status"] == "exited",
                          f"application termination ({reason})")
            assert exited == {**desktop, "application": {"status": "exited", **termination}, "capture": ready}, exited
            assert client.call("desktop_status") == exited

        (worker,) = children(client.process.pid)
        capture = child_executable(worker, "gst-launch-1.0")
        os.kill(capture, signal.SIGKILL)
        down = poll(lambda: client.call("desktop_status"), lambda s: s["capture"]["status"] == "recovery_needed",
                    "capture process exit")
        assert down == {**exited, "capture": {"status": "recovery_needed"}}, down
        request.unlink()
        assert client.call("launch_app") == running
        assert_running_app(client, "recovery_needed")
        client.call("press_key", {"key": "a"}, error="private capture stream closed")
        client.call("type_text", {"text": "test"}, error="private capture stream closed")
        assert client.call("screenshot")["frame_id"]
        assert_running_app(client, "ready")

        with stopped_process(child_executable(worker, "pipewire")):
            status = client.call("desktop_status")
            assert status["status"] == "application_running" and status["application"] == {"status": "running"}
            assert status["capture"]["status"] == "unknown" and status["capture"]["error"], status

        os.kill(child_executable(worker, "wireplumber"), signal.SIGTERM)
        failed = poll(lambda: client.call("desktop_status"), lambda s: s["status"] == "failed", "desktop service exit")
        assert failed["capture"] == {"status": "unavailable"} and failed["application"] == {"status": "running"}
        assert client.call("desktop_stop") == {**desktop, "status": "stopped"}
        assert client.call("desktop_status") == initial
    finally:
        client.close()


def exercise(client, report):
    initialize(client)
    tools = client.send("tools/list", {})["tools"]
    assert {tool["name"] for tool in tools} == {
        "desktop_start", "desktop_status", "desktop_stop", "launch_app", "screenshot",
        "mouse_move", "mouse_click", "mouse_drag", "mouse_scroll", "press_key", "type_text",
    }
    assert client.call("desktop_status")["status"] == "stopped"
    client.call("screenshot", error="start the private desktop")
    started = client.call("desktop_start")
    assert started["status"] == "desktop_running" and started["mode"] == "headless"
    assert client.call("launch_app")["status"] == "application_running"
    app = poll(lambda: snapshot(report), lambda state: state.get("ready"), "GTK window mapped")
    runtime = Path(app["runtime"])
    assert runtime.is_dir() and runtime.stat().st_mode & 0o777 == 0o700
    assert (runtime / "mcp-wayland").is_socket() and (runtime / "pipewire-0").is_socket()
    assert app["bus"] == f"unix:path={runtime}/bus" and (runtime / "bus").is_socket()
    assert app["pipewire_runtime"] == str(runtime)
    assert app["wayland"] == "mcp-wayland" and app["x11"] is None and app["wayland_socket"] is None

    frame = poll(lambda: client.call("screenshot"), lambda frame: frame["pixel"] == (38, 64, 90), "screenshot contains GTK surface")
    assert (frame["width"], frame["height"]) == (960, 540)
    client.call("mouse_move", point(frame, 960, 100), error="outside the screenshot")
    client.call("mouse_move", point(frame, 170, 135))
    poll(lambda: snapshot(report), lambda state: state.get("motion", 0) >= 1, "private mouse movement")
    client.call("mouse_click", {"point": point(frame, 170, 135)}, error="stale or unknown")
    assert snapshot(report)["clicks"] == 0
    frame = client.call("screenshot")
    client.call("mouse_click", {"point": point(frame, 170, 135)})
    poll(lambda: snapshot(report), lambda state: state.get("clicks") == 1, "GTK click handler")
    frame = client.call("screenshot")
    client.call("mouse_scroll", {"point": point(frame, 220, 375), "vertical": 5})
    poll(lambda: snapshot(report), lambda state: state.get("scroll", 0) > 0, "GTK scrollbar moved")
    frame = client.call("screenshot")
    client.call("mouse_click", {"point": point(frame, 490, 138)})
    client.call("press_key", {"key": "a"})
    poll(lambda: snapshot(report), lambda state: state.get("text") == "a", "GTK received key a")
    literal = "\ud55c\uae00 \ud14c\uc2a4\ud2b8 · café 🌱\n"
    expected = "a"
    for index, options in enumerate(({}, {"paste_shortcut": "ctrl_v"}, {"paste_shortcut": None}), 1):
        client.call("type_text", {"text": literal, **options})
        expected += literal
        poll(lambda: snapshot(report),
             lambda state: state.get("ctrl_pastes") == index and state.get("text") == expected,
             "GTK received exactly one Ctrl+V and the multiline Unicode text")
    assert snapshot(report)["shift_pastes"] == 0

    frame = client.call("screenshot")
    client.call("type_text", {"text": "unexpected", "paste_shortcut": "invalid"}, error="unknown variant")
    client.call("mouse_move", point(frame, 490, 138))
    client.call("press_key", {"key": "v", "modifiers": ["ctrl"]})
    expected += literal
    poll(lambda: snapshot(report),
         lambda state: state.get("ctrl_pastes") == 4 and state.get("text") == expected,
         "GTK can still paste the original private clipboard after rejected arguments")

    shifted = {"text": literal, "paste_shortcut": "ctrl_shift_v"}
    client.call("type_text", shifted)
    expected += literal
    poll(lambda: snapshot(report),
         lambda state: state.get("shift_pastes") == 1 and state.get("text") == expected,
         "GTK received exactly one Ctrl+Shift+V and pasted the repeated text")
    assert snapshot(report)["ctrl_pastes"] == 4
    client.call("press_key", {"key": "x"})
    expected += "x"
    poll(lambda: snapshot(report), lambda state: state.get("text") == expected,
         "Ctrl and Shift were released after paste")

    client.call("press_key", {"key": "F6"})
    poll(lambda: snapshot(report), lambda state: state.get("paste_deferred"), "GTK deferred the next paste")
    assert client.call("type_text", shifted)["status"] == "dispatched"
    client.call("press_key", {"key": "Enter"})
    paste_result = poll(lambda: snapshot(report),
                        lambda state: state.get("shift_pastes") == 2 and state.get("text") == expected + literal,
                        "GTK inserted the second shifted paste after Enter")
    assert paste_result["text_at_enter"] == expected
    assert paste_result["ctrl_pastes"] == 4
    client.call("press_key", {"key": "a", "modifiers": ["ctrl"]})
    client.call("press_key", {"key": "Backspace"})
    poll(lambda: snapshot(report), lambda state: state.get("text") == "", "GTK received Ctrl+A and Backspace")

    exercise_capture_interruption(client, report)
    frame = client.call("screenshot")
    assert client.call("desktop_stop")["status"] == "stopped"
    assert not runtime.exists() and not Path(f"/proc/{app['pid']}").exists()
    client.call("press_key", {"key": "a"}, error="start the private desktop")
    assert client.call("desktop_start")["status"] == "desktop_running"
    client.call("mouse_move", point(frame, 170, 135), error="stale or unknown")
    report.unlink()
    client.call("launch_app")
    second = poll(lambda: snapshot(report), lambda state: state.get("ready"), "second GTK window mapped")
    assert second["runtime"] != str(runtime)
    exercise_capture_link_loss(client, report, second)
    client.process.send_signal(signal.SIGTERM)
    client.process.wait(timeout=10)
    assert client.process.returncode == 0
    assert not Path(second["runtime"]).exists() and not Path(f"/proc/{second['pid']}").exists()


def runtime_processes(runtime):
    marker = os.fsencode(f"XDG_RUNTIME_DIR={runtime}")
    live = set()
    for process in Path("/proc").iterdir():
        if process.name.isdigit():
            try:
                if marker in (process / "environ").read_bytes().split(b"\0"):
                    live.add(int(process.name))
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
    return live


def disconnect_capture_link(runtime):
    remote = runtime / "pipewire-0"
    assert runtime.parent == Path("/tmp") and runtime.name.startswith("gwm-") and remote.is_socket()
    objects = json.loads(subprocess.check_output(["/usr/bin/pw-dump", "-r", remote], timeout=5))
    nodes = {obj["id"]: obj["info"]["props"].get("node.name") for obj in objects
             if obj["type"] == "PipeWire:Interface:Node"}
    (link,) = [obj for obj in objects if obj["type"] == "PipeWire:Interface:Link"
               and nodes[obj["info"]["output-node-id"]] == "mutter"
               and nodes[obj["info"]["input-node-id"]] == "gst-launch-1.0"]
    subprocess.run(["/usr/bin/pw-link", "-r", remote, "-d", str(link["id"])], check=True, timeout=5)


@contextmanager
def stopped_process(pid):
    handle = os.pidfd_open(pid)
    try:
        signal.pidfd_send_signal(handle, signal.SIGSTOP)

        def process_state():
            return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0]

        poll(process_state, lambda state: state == "T", "private process stopped")
        yield
    finally:
        try:
            signal.pidfd_send_signal(handle, signal.SIGCONT)
        except ProcessLookupError:
            pass  # Capture recovery already reaped this process.
        os.close(handle)


def click_after_unlink(client, runtime, app_pid, frame):
    handle = os.pidfd_open(app_pid)
    try:
        disconnect_capture_link(runtime)
        # Issue the old click before waiting for GTK, a new PNG or gst-launch's exit.
        signal.pidfd_send_signal(handle, signal.SIGUSR1)
        result = client.send("tools/call", {"name": "mouse_click", "arguments": {"point": point(frame, 170, 135)}})
    finally:
        os.close(handle)
    # Require rejection before any pointer movement or partial dispatch.
    error = result["content"][0]["text"]
    assert result.get("isError") and error == "Click was not sent: private capture was interrupted; take a new screenshot", error


def exercise_capture_link_loss(client, report, app):
    runtime = Path(app["runtime"])
    frame = poll(lambda: client.call("screenshot"), lambda f: f["pixel"] == (38, 64, 90), "original GTK screen")
    client.call("mouse_click", {"point": point(frame, 490, 138)})
    text = "preserve this entry"
    client.call("type_text", {"text": text})
    poll(lambda: snapshot(report), lambda s: s["text"] == text, "text before capture disconnect")
    frame = client.call("screenshot")
    assert frame["pixel"] == (38, 64, 90)
    (worker,) = children(client.process.pid)
    before = set(children(worker))
    assert app["pid"] in before
    capture = child_executable(worker, "gst-launch-1.0")
    monitor = child_executable(worker, "pw-dump")
    click_after_unlink(client, runtime, app["pid"], frame)
    poll(lambda: snapshot(report), lambda s: s.get("target_replaced"), "target changed with the link down")
    assert_running_app(client, "recovery_needed")

    recovered = client.call("screenshot")
    assert_running_app(client, "ready")
    assert recovered["frame_id"] != frame["frame_id"] and recovered["pixel"] == (97, 45, 69), recovered
    after = set(children(worker))
    assert before - after == {capture, monitor}
    new_processes = {Path(f"/proc/{pid}/exe").readlink().name: pid for pid in after - before}
    assert set(new_processes) == {"gst-launch-1.0", "pw-dump"} and len(after) == len(before)
    replacement = new_processes["gst-launch-1.0"]
    client.call("mouse_click", {"point": point(frame, 170, 135)}, error="stale or unknown")
    assert snapshot(report).get("replacement_clicks", 0) == 0
    client.call("mouse_click", {"point": point(recovered, 170, 135)})
    state = poll(lambda: snapshot(report), lambda s: s.get("replacement_clicks") == 1, "click after capture recovery")
    assert state["pid"] == app["pid"] and state["text"] == text and state["clicks"] == 0

    # Keep the old reader alive with stdout open throughout the graph check.
    frame = client.call("screenshot")
    with stopped_process(replacement):
        click_after_unlink(client, runtime, app["pid"], frame)
        poll(lambda: snapshot(report), lambda s: s.get("target_replaced") is False, "original button restored")
        assert_running_app(client, "recovery_needed")
        recovered = client.call("screenshot")
    assert frame["pixel"] == (97, 45, 69) and recovered["pixel"] == (38, 64, 90)
    assert recovered["frame_id"] != frame["frame_id"] and not Path(f"/proc/{replacement}").exists()
    state = snapshot(report)
    assert state["pid"] == app["pid"] and state["text"] == text
    assert state["clicks"] == 0 and state["replacement_clicks"] == 1

    # A repeated desktop_start must also repair capture and invalidate an unused frame ID.
    frame = recovered
    disconnect_capture_link(runtime)
    assert client.call("desktop_start")["status"] == "application_running"
    assert_running_app(client, "ready")
    client.call("mouse_click", {"point": point(frame, 170, 135)}, error="stale or unknown")
    recovered = client.call("screenshot")
    assert recovered["pixel"] == (38, 64, 90) and recovered["frame_id"] != frame["frame_id"]
    client.call("mouse_click", {"point": point(recovered, 170, 135)})
    state = poll(lambda: snapshot(report), lambda s: s["clicks"] == 1, "click after desktop_start")
    assert state["pid"] == app["pid"] and state["text"] == text

    # Lose only the monitor while the original link and GStreamer are healthy.
    frame = client.call("screenshot")
    (monitor,) = [pid for pid in children(worker)
                  if b"--monitor" in Path(f"/proc/{pid}/cmdline").read_bytes().split(b"\0")]
    handle = os.pidfd_open(monitor)
    try:
        signal.pidfd_send_signal(handle, signal.SIGTERM)
        poll(lambda: monitor in children(worker), lambda alive: not alive, "capture link monitor exited")
    finally:
        os.close(handle)
    client.call("mouse_click", {"point": point(frame, 170, 135)}, error="Click was not sent")
    recovered = client.call("screenshot")
    assert recovered["pixel"] == frame["pixel"] and recovered["frame_id"] != frame["frame_id"]
    assert snapshot(report)["clicks"] == 1 and snapshot(report)["pid"] == app["pid"]


def exercise_capture_interruption(client, report):
    original = client.call("screenshot")
    still = client.call("screenshot")
    assert original["pixel"] == still["pixel"] == (38, 64, 90)
    assert original["frame_id"] != still["frame_id"]
    client.call("mouse_click", {"point": point(still, 170, 135)})
    poll(lambda: snapshot(report), lambda state: state["clicks"] == 2, "click on a static screen")

    (worker,) = children(client.process.pid)
    frame = client.call("screenshot")
    with stopped_process(child_executable(worker, "gst-launch-1.0")):
        assert_running_app(client, "paused")
        assert client.call("desktop_start")["status"] == "application_running"
        assert_running_app(client, "paused")
        client.call("mouse_click", {"point": point(frame, 170, 135)}, error="private capture is paused")
        client.call("press_key", {"key": "F7"})
        poll(lambda: snapshot(report), lambda state: state.get("target_replaced"), "GTK replaced the click target")
        result = client.send("tools/call", {"name": "screenshot", "arguments": {}})
        assert result.get("isError") is True and len(result["content"]) == 1
        error = result["content"][0]
        assert error["type"] == "text" and "private capture is paused" in error["text"]
        assert "frame_id" not in error["text"]
        effects = snapshot(report)
        assert effects["clicks"] == 2 and effects.get("replacement_clicks", 0) == 0

    assert_running_app(client, "recovery_needed")
    recovered = client.call("screenshot")
    assert recovered["pixel"] == (97, 45, 69)
    client.call("mouse_click", {"point": point(frame, 170, 135)}, error="stale or unknown")
    client.call("mouse_click", {"point": point(recovered, 170, 135)})
    poll(lambda: snapshot(report), lambda state: state.get("replacement_clicks") == 1,
         "click on the replacement target after capture resumes")

    # Detect stop/continue even when no tool was called while it was stopped,
    # and obtain a fresh frame without changing the static app.
    frame = client.call("screenshot")
    with stopped_process(child_executable(worker, "gst-launch-1.0")):
        pass  # Resume without an intervening MCP call.
    client.call("mouse_click", {"point": point(frame, 170, 135)}, error="private capture was interrupted")
    recovered = client.call("screenshot")
    assert recovered["pixel"] == (97, 45, 69)
    client.call("mouse_click", {"point": point(recovered, 170, 135)})
    poll(lambda: snapshot(report), lambda state: state.get("replacement_clicks") == 2,
         "click on static screen after unobserved capture interruption")


def children(pid):
    return [int(pid) for pid in Path(f"/proc/{pid}/task/{pid}/children").read_text().split()]


def child_executable(parent, name):
    (pid,) = [pid for pid in children(parent) if Path(f"/proc/{pid}/exe").resolve() == Path("/usr/bin", name)]
    return pid


def stall_private_mutter(worker_pid):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        for pid in children(worker_pid):
            proc = Path(f"/proc/{pid}")
            try:
                if (proc / "exe").readlink() != Path("/usr/bin/mutter"):
                    continue
                environment = (proc / "environ").read_bytes().split(b"\0")
                marker = next(value for value in environment if value.startswith(b"XDG_RUNTIME_DIR=/tmp/gwm-"))
                handle = os.pidfd_open(pid)
            except (FileNotFoundError, ProcessLookupError, StopIteration):
                continue
            try:
                signal.pidfd_send_signal(handle, signal.SIGSTOP)
                return Path(os.fsdecode(marker.split(b"=", 1)[1]))
            finally:
                os.close(handle)
        time.sleep(0.001)
    raise AssertionError("Did not find the private Mutter during startup")


def exercise_launcher_loss(client, report, *, interrupt_start=False, close_stdin=False):
    initialize(client)
    launcher = client.process.pid
    (worker,) = children(launcher)
    args = Path(f"/proc/{worker}/cmdline").read_bytes().split(b"\0")
    assert args[1:3] == [b"--worker-parent-pid", str(launcher).encode()]
    assert os.getsid(worker) != os.getsid(launcher)
    worker_handle = os.pidfd_open(worker)
    worker_exit = select.poll()
    worker_exit.register(worker_handle, select.POLLIN)
    runtime = None

    try:
        if interrupt_start:
            client.write("tools/call", {"name": "desktop_start", "arguments": {}})
            candidate = stall_private_mutter(worker)
        else:
            assert client.call("desktop_start")["status"] == "desktop_running"
            assert client.call("launch_app")["status"] == "application_running"
            app = poll(lambda: snapshot(report), lambda state: state.get("ready"), "GTK window for shutdown test")
            candidate = Path(app["runtime"])
        assert candidate.parent == Path("/tmp") and candidate.name.startswith("gwm-")
        assert (candidate / "bus.conf").is_file()
        runtime = candidate
        if interrupt_start:
            assert client.lines.empty(), "desktop_start had already replied"
        else:
            assert app["pid"] in runtime_processes(runtime)

        if close_stdin:
            client.process.stdin.close()
        else:
            # Keep stdin open so EOF cannot satisfy the launcher-loss test.
            os.killpg(launcher, signal.SIGKILL)
            assert client.process.wait(timeout=3) == -signal.SIGKILL
        poll(lambda: (runtime.exists(), runtime_processes(runtime), bool(worker_exit.poll(0))),
             lambda state: state == (False, set(), True),
             "private runtime and worker exit after shutdown", timeout=3)
        if close_stdin:
            assert client.process.wait(timeout=3) == 0
    finally:
        # A failure must not leave the stopped test compositor or worker alive.
        if runtime:
            for pid in runtime_processes(runtime):
                try:
                    handle = os.pidfd_open(pid)
                    try:
                        if pid in runtime_processes(runtime):
                            signal.pidfd_send_signal(handle, signal.SIGKILL)
                    finally:
                        os.close(handle)
                except ProcessLookupError:
                    pass
            if not worker_exit.poll(500):
                try:
                    signal.pidfd_send_signal(worker_handle, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            poll(lambda: runtime_processes(runtime), lambda live: not live, "test process teardown")
            if runtime.exists():
                shutil.rmtree(runtime)
        os.close(worker_handle)


def main():
    binary = str(Path(sys.argv[1]).resolve())
    fixture = str(Path(__file__).with_name("gtk_fixture.py").resolve())
    with tempfile.TemporaryDirectory(prefix="gwm-smoke-") as directory:
        root = Path(directory)
        log_path = root / "mcp.log"
        with log_path.open("w") as log:
            try:
                exercise_application_status(binary, root, log)
            except Exception:
                print(log_path.read_text(encoding="utf-8"), file=sys.stderr)
                raise

            client = Client(binary, [sys.executable, fixture, root / "gtk.json"], log)
            try:
                exercise(client, root / "gtk.json")
            except Exception:
                print(log_path.read_text(encoding="utf-8"), file=sys.stderr)
                raise
            finally:
                client.close()

            for scenario, options in (
                ("running", {}),
                ("startup-sigkill", {"interrupt_start": True}),
                ("startup-eof", {"interrupt_start": True, "close_stdin": True}),
            ):
                report = root / f"{scenario}.json"
                client = Client(binary, [sys.executable, fixture, report], log, own_process_group=True)
                try:
                    exercise_launcher_loss(client, report, **options)
                except Exception:
                    print(log_path.read_text(encoding="utf-8"), file=sys.stderr)
                    raise
                finally:
                    client.close()
    print("Passed: MCP, desktop/app/capture status, app termination, private sockets, PNG, pointer, click, scroll, "
          "keys, paste shortcuts and async insertion, stale frame, first PNG and same app after capture recovery, "
          "stop/restart, SIGTERM, launcher loss, interrupted startup, worker exit")


if __name__ == "__main__":
    main()
