"""Compare two MCP servers and the wbox startup delay on one Linux host."""
import argparse
import base64
from collections import Counter
from contextlib import suppress
import io
from itertools import permutations
import json
import os
from pathlib import Path
import queue
import select
import signal
from statistics import median
import subprocess
import threading
import time
from uuid import uuid4

from PIL import Image


FIXTURE = Path(__file__).with_name("gtk_clipboard.py").resolve()
WIDTH, HEIGHT = 960, 540
ORIGINAL = (38, 64, 90)
RECOVERED = (97, 45, 69)
KOREAN = "한글 붙여넣기 테스트\n둘째 줄: 가나다와 띄어쓰기"
CASES = ("gwm", "wbox-default", "wbox-no-wait")
PAUSE_SECONDS = 6


def poll(read, accept, seconds, description):
    end = time.monotonic() + seconds
    while True:
        current = read()
        if accept(current):
            return current
        if time.monotonic() >= end:
            raise RuntimeError(f"{description}: {current!r}")
        time.sleep(0.02)


def process_markers(token, session_runtime):
    markers = {b"BENCHMARK_TRIAL=" + os.fsencode(token)}
    if session_runtime is not None:
        markers.add(b"PIPEWIRE_RUNTIME_DIR=" + os.fsencode(session_runtime))
    return markers


def processes(token, session_runtime):
    markers = process_markers(token, session_runtime)
    matches = {}
    for entry in Path("/proc").iterdir():
        if not entry.name.isdecimal():
            continue
        try:
            if markers.intersection((entry / "environ").read_bytes().split(b"\0")):
                matches[int(entry.name)] = (entry / "exe").readlink().name
        except (FileNotFoundError, PermissionError, ProcessLookupError):
            continue
    return matches


class MCP:
    def __init__(self, command, directory, environment):
        with (directory / "server.log").open("wb") as log:
            self.proc = subprocess.Popen(
                command, cwd=directory, env=environment, stdin=subprocess.PIPE,
                stdout=subprocess.PIPE, stderr=log, start_new_session=True,
            )
        self.lines = queue.Queue()
        self.counter = 0
        self.reader = threading.Thread(target=self._read_lines, daemon=True)
        self.reader.start()

    def _read_lines(self):
        for line in self.proc.stdout:
            self.lines.put(line)
        self.lines.put(None)

    def request(self, method, params=None, timeout=10, notification=False):
        self.counter += 1
        request = {"jsonrpc": "2.0", "method": method}
        if not notification:
            request["id"] = self.counter
        if params is not None:
            request["params"] = params
        self.proc.stdin.write(json.dumps(request).encode() + b"\n")
        self.proc.stdin.flush()
        if notification:
            return
        deadline = time.monotonic() + timeout
        label = (params or {}).get("name", method)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError(f"MCP timeout in {label}")
            try:
                line = self.lines.get(timeout=remaining)
            except queue.Empty as error:
                raise RuntimeError(f"MCP timeout in {label}") from error
            if line is None:
                raise RuntimeError(f"MCP disconnected in {method}: exit={self.proc.poll()}")
            response = json.loads(line)
            if response.get("id") == self.counter:
                if "error" in response:
                    raise RuntimeError(str(response["error"]))
                return response["result"]

    def call(self, name, args=None, timeout=10):
        return self.request("tools/call", {"name": name, "arguments": args or {}}, timeout)

    def close(self):
        if not self.proc.stdin.closed:
            self.proc.stdin.close()
        self.proc.wait(timeout=8)
        self.reader.join(timeout=2)
        self.proc.stdout.close()


def error_text(result):
    messages = [entry["text"] for entry in result.get("content", []) if entry["type"] == "text"]
    return "\n".join(messages)


def checked(result):
    message = error_text(result)
    if result.get("isError") or message.startswith(("Error:", "Input validation error")):
        raise RuntimeError(message or str(result))
    return message


def pixels(result):
    images = [entry for entry in result.get("content", []) if entry["type"] == "image"]
    if result.get("isError") or len(images) != 1 or images[0]["mimeType"] != "image/png":
        raise RuntimeError(error_text(result) or "MCP response lacked a PNG")
    png = base64.b64decode(images[0]["data"], validate=True)
    with Image.open(io.BytesIO(png)) as image:
        image.load()
        if image.format != "PNG" or image.size != (WIDTH, HEIGHT):
            raise RuntimeError(f"unexpected capture {image.format} {image.size}")
        marker = image.convert("RGB").getpixel((30, 30))
    return marker, png


def app_state(directory):
    path = directory / "app.json"
    return json.loads(path.read_text()) if path.exists() else {}


def pid_done(fd):
    return bool(select.select([fd], [], [], 0)[0])


def fallback_cleanup(token, session_runtime):
    markers = process_markers(token, session_runtime)
    for pid in processes(token, session_runtime):
        try:
            fd = os.pidfd_open(pid)
            try:
                environ = Path(f"/proc/{pid}/environ").read_bytes().split(b"\0")
                if markers.intersection(environ):
                    signal.pidfd_send_signal(fd, signal.SIGKILL)
            finally:
                os.close(fd)
        except (FileNotFoundError, ProcessLookupError):
            continue


def run_trial(case, binary, directory):
    directory.mkdir(parents=True)
    runtime = directory / "rt"
    runtime.mkdir(mode=0o700)
    env = os.environ.copy()
    for name in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DBUS_SESSION_BUS_ADDRESS",
                 "PIPEWIRE_RUNTIME_DIR", "PULSE_SERVER", "PIPEWIRE_REMOTE"):
        env.pop(name, None)
    token = uuid4().hex
    env.update(BENCHMARK_TRIAL=token, XDG_RUNTIME_DIR=str(runtime))
    app = ["/usr/bin/python3", "-B", str(FIXTURE), str(directory)]
    is_gwm = case == "gwm"
    if is_gwm:
        command = [str(binary), "--width", str(WIDTH), "--height", str(HEIGHT), "--", *app]
        stop_tool = "desktop_stop"
    else:
        config = directory / "wbox.json"
        settings = {
            "compositor": "mutter", "headless": True, "audio": "null",
            "clipboard_bridge": False, "keyboard_layout": "us",
            "screen": f"{WIDTH}x{HEIGHT}", "name": f"gwm-bench-{token}",
            "app": {"command": app},
        }
        if case == "wbox-no-wait":
            settings["timeouts"] = {"app_render": 0}
        config.write_text(json.dumps(settings), encoding="utf-8")
        command = [str(binary), "serve", str(config)]
        stop_tool = "stop"

    result = {"case": case, "trial": directory.name, "ok": False}
    fds = {}
    mcp = None
    session_runtime = None
    stage = "initialize"
    started = time.monotonic()
    try:
        mcp = MCP(command, directory, env)
        hello = mcp.request("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                          "clientInfo": {"name": "bench", "version": "1"}}, 12)
        result["server_info"] = hello["serverInfo"]
        mcp.request("notifications/initialized", notification=True)
        stage = "launch"
        if is_gwm:
            checked(mcp.call("desktop_start", timeout=22))
            checked(mcp.call("launch_app"))
        else:
            checked(mcp.call("launch", timeout=22))
        result["launch_reply_s"] = time.monotonic() - started
        ready = poll(lambda: app_state(directory), lambda state: state.get("ready"), 6, "GTK ready")
        result["ready_s"] = time.monotonic() - started
        private = Path(ready["session_runtime"])
        prefix = "gwm-" if is_gwm else "wbox-a-"
        if private.parent != Path("/tmp") or not private.name.startswith(prefix) or private.is_symlink():
            raise RuntimeError(f"unexpected private runtime path {private}")
        session_runtime = private
        stage = "capture process identification"
        candidates = processes(token, session_runtime)
        capture_pid, = (pid for pid, name in candidates.items() if name == "gst-launch-1.0")
        compositor_pid, = (pid for pid, name in candidates.items() if name == "mutter")
        if ready["pid"] not in candidates:
            raise RuntimeError("GTK fixture does not belong to the private session")
        for name, pid in (("app", ready["pid"]), ("capture", capture_pid), ("mutter", compositor_pid)):
            fds[name] = os.pidfd_open(pid)
        stage = "baseline screenshot"
        baseline_deadline = time.monotonic() + 5
        first_attempts = 0
        while True:
            first_attempts += 1
            marker, png = pixels(mcp.call("screenshot", timeout=baseline_deadline - time.monotonic()))
            if marker == ORIGINAL:
                break
            if time.monotonic() >= baseline_deadline:
                raise RuntimeError(f"incorrect baseline marker: {marker}")
            time.sleep(0.05)
        result["baseline_requests"] = first_attempts
        result["first_frame_s"] = time.monotonic() - started
        result["frame_wait_s"] = result["first_frame_s"] - result["launch_reply_s"]
        (directory / "before.png").write_bytes(png)

        stage = "paste"
        paste_started = time.monotonic()
        if is_gwm:
            checked(mcp.call("type_text", {"text": KOREAN}, timeout=6))
        else:
            checked(mcp.call("clipboard_write", {"text": KOREAN}, timeout=6))
            checked(mcp.call("hold", {"keys": ["ctrl"], "actions": [{"type": "key", "key": "v"}]}, timeout=6))
        poll(lambda: app_state(directory).get("text"),
             lambda text: text == KOREAN, 4, "exact Korean paste")
        result["paste_s"] = time.monotonic() - paste_started
        result["first_paste_s"] = time.monotonic() - started

        stage = "capture interruption"
        capture = fds["capture"]
        signal.pidfd_send_signal(capture, signal.SIGSTOP)
        stopped = time.monotonic()
        resume_at = stopped + PAUSE_SECONDS
        try:
            def stopped_state():
                return Path(f"/proc/{capture_pid}/stat").read_text().rsplit(")", 1)[1].split()[0]

            poll(stopped_state, lambda state: state == "T", 1, "GStreamer stopped")
            requested = time.monotonic()
            fault = mcp.call("screenshot", timeout=resume_at - requested)
            replied = time.monotonic()
            result["fault_reply_s"] = replied - requested
            result["fault_message"] = error_text(fault)
            expected = "capture is paused" if is_gwm else "no frame from mutter"
            if any(item["type"] == "image" for item in fault["content"]) or expected not in error_text(fault):
                raise RuntimeError(f"incorrect paused-capture response: {error_text(fault)}")
            if replied >= resume_at:
                raise RuntimeError("no error before the capture resume deadline")
        finally:
            try:
                time.sleep(max(0, resume_at - time.monotonic()))
            finally:
                signal.pidfd_send_signal(capture, signal.SIGCONT)
        resumed = time.monotonic()
        result["pause_s"] = resumed - stopped
        result["post_error_wait_s"] = resumed - replied

        stage = "capture recovery"
        (directory / "flip-marker").touch()
        poll(lambda: app_state(directory), lambda state: state.get("marker") == "after",
             1, "GTK rendered a new marker after capture resumed")
        deadline = resumed + 10
        attempt = 0
        while True:
            attempt += 1
            marker, png = pixels(mcp.call("screenshot", timeout=deadline - time.monotonic()))
            if marker == RECOVERED:
                break
            if time.monotonic() >= deadline or attempt == 3:
                raise RuntimeError(f"stale PNG after capture resume: {marker}")
        recovered = app_state(directory)
        if recovered.get("text") != KOREAN or recovered.get("pid") != ready["pid"]:
            raise RuntimeError("GTK fixture or text changed after resume")
        result["recover_s"] = time.monotonic() - resumed
        result["recover_requests"] = attempt
        final_capture, = (pid for pid, name in processes(token, session_runtime).items()
                          if name == "gst-launch-1.0")
        result["capture_recreated"] = final_capture != capture_pid
        if final_capture != capture_pid:
            fds["replacement capture"] = os.pidfd_open(final_capture)
        (directory / "after.png").write_bytes(png)

        stage = "shutdown"
        shutdown_started = time.monotonic()
        checked(mcp.call(stop_tool, timeout=12))
        poll(lambda: [name for name, fd in fds.items() if not pid_done(fd)],
             lambda live: not live, 4, "app, capture and Mutter exited")
        mcp.close()
        if mcp.proc.returncode != 0:
            raise RuntimeError(f"nonzero MCP exit: {mcp.proc.returncode}")
        poll(lambda: processes(token, session_runtime), lambda live: not live, 3, "remaining trial processes")
        if session_runtime.exists():
            raise RuntimeError("private session runtime directory remained after stop")
        result["shutdown_s"] = time.monotonic() - shutdown_started
        result["ok"] = True
    except Exception as error:
        result.update(stage=stage, failure=str(error))
    finally:
        try:
            if not result["ok"] and mcp is not None:
                if mcp.proc.poll() is None and not mcp.proc.stdin.closed:
                    with suppress(OSError, RuntimeError, ValueError):
                        mcp.call(stop_tool, timeout=5)
                try:
                    mcp.close()
                except subprocess.TimeoutExpired:
                    fallback_cleanup(token, session_runtime)
                    mcp.close()
                fallback_cleanup(token, session_runtime)
                poll(lambda: processes(token, session_runtime), lambda live: not live, 3,
                     "remaining processes after failed trial")
                if session_runtime is not None and session_runtime.exists():
                    raise RuntimeError(f"private runtime remained after failed trial: {session_runtime}")
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            result["cleanup_failure"] = str(error)
        finally:
            for fd in fds.values():
                os.close(fd)
    result["elapsed_s"] = time.monotonic() - started
    return result


def summarize(trials):
    report = {}
    metrics = ("launch_reply_s", "ready_s", "first_frame_s", "frame_wait_s", "first_paste_s",
               "paste_s", "fault_reply_s", "recover_s", "shutdown_s", "elapsed_s")
    for case in CASES:
        group = [row for row in trials if row["case"] == case]
        successes = [row for row in group if row["ok"]]
        first = next((index for index, row in enumerate(group) if row["ok"]), None)
        report[case] = {
            "trials": len(group),
            "failures": len(group) - len(successes),
            "failed_stages": dict(Counter(row["stage"] for row in group if not row["ok"])),
            "first_success_trial": first + 1 if first is not None else None,
            "first_success_s": (
                sum(row["elapsed_s"] for row in group[:first + 1]) if first is not None else None
            ),
            "median_s": {
                metric: median(row[metric] for row in successes) for metric in metrics
            } if successes else {},
            "elapsed_range_s": (
                [min(row["elapsed_s"] for row in successes), max(row["elapsed_s"] for row in successes)]
                if successes else None
            ),
        }
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gnome_wayland_mcp", type=Path)
    parser.add_argument("wbox_mcp", type=Path)
    parser.add_argument("--rounds", type=int, default=12, help="trials per condition")
    parser.add_argument("--output", type=Path, required=True, help="new directory for results and logs")
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error("--rounds must be positive")
    gwm = args.gnome_wayland_mcp.resolve()
    wbox = args.wbox_mcp.resolve()
    for binary in (gwm, wbox):
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"not an executable file: {binary}")
    output = args.output.resolve()
    output.mkdir()
    orders = list(permutations(CASES))
    versions = [subprocess.check_output([binary, "--version"], text=True, timeout=10).strip()
                for binary in (gwm, wbox)]
    trials = []
    with (output / "trials.jsonl").open("w", encoding="utf-8") as log:
        for index in range(args.rounds):
            for case in orders[index % len(orders)]:
                trial = output / f"{index + 1:02d}-{case}"
                row = run_trial(case, gwm if case == "gwm" else wbox, trial)
                trials.append(row)
                line = json.dumps(row, ensure_ascii=False)
                log.write(line + "\n")
                log.flush()
                print(line, flush=True)
                if row.get("cleanup_failure"):
                    raise SystemExit(f"Stopped after a cleanup failure in {trial}")
    report = {"versions": versions, "pause_s": PAUSE_SECONDS, "results": summarize(trials)}
    data = json.dumps(report, indent=2)
    (output / "summary.json").write_text(data + "\n", encoding="utf-8")
    print(data)
    return int(any(not row["ok"] for row in trials))


if __name__ == "__main__":
    raise SystemExit(main())
