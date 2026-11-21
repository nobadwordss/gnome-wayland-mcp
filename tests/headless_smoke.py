"""Exercise the real MCP transport, a private Mutter desktop and a GTK client."""

import base64
import io
import json
import os
from pathlib import Path
from queue import Queue, Empty
import signal
import subprocess
import sys
import tempfile
from threading import Thread
import time

from PIL import Image


class Client:
    def __init__(self, binary, fixture, report, log):
        self.process = subprocess.Popen(
            [binary, "--width", "960", "--height", "540", "--", sys.executable, fixture, report],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=log,
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

    def send(self, method, params=None, notification=False):
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        if not notification:
            self.request_id += 1
            message["id"] = self.request_id
        self.process.stdin.write(json.dumps(message).encode() + b"\n")
        self.process.stdin.flush()
        if notification:
            return None
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

    def call(self, tool, args=None, error=None):
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
                output["pixel"] = png.convert("RGB").getpixel((30, 30))
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


def exercise(client, report):
    init = client.send("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
                                      "clientInfo": {"name": "headless-smoke", "version": "1"}})
    assert init["serverInfo"]["name"] == "gnome-wayland-mcp"
    client.send("notifications/initialized", notification=True)
    tools = client.send("tools/list", {})["tools"]
    assert {tool["name"] for tool in tools} == {
        "desktop_start", "desktop_status", "desktop_stop", "launch_app", "screenshot",
        "mouse_move", "mouse_click", "mouse_scroll", "press_key", "type_text",
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
    literal = "\ud55c\uae00 \ud14c\uc2a4\ud2b8 · café 🌱"
    client.call("type_text", {"text": literal})
    poll(lambda: snapshot(report), lambda state: state.get("text") == "a" + literal, "GTK received Unicode paste")
    client.call("type_text", {"text": " / \uc0c8 \ud14d\uc2a4\ud2b8"})
    poll(lambda: snapshot(report), lambda state: state.get("text") == "a" + literal + " / \uc0c8 \ud14d\uc2a4\ud2b8",
         "GTK received the replacement private clipboard selection")
    client.call("press_key", {"key": "a", "modifiers": ["ctrl"]})
    client.call("press_key", {"key": "Backspace"})
    poll(lambda: snapshot(report), lambda state: state.get("text") == "", "GTK received Ctrl+A and Backspace")

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
    client.process.send_signal(signal.SIGTERM)
    client.process.wait(timeout=10)
    assert client.process.returncode == 0
    assert not Path(second["runtime"]).exists() and not Path(f"/proc/{second['pid']}").exists()


def main():
    binary = str(Path(sys.argv[1]).resolve())
    fixture = str(Path(__file__).with_name("gtk_fixture.py").resolve())
    with tempfile.TemporaryDirectory(prefix="gwm-smoke-") as directory:
        root = Path(directory)
        log_path = root / "mcp.log"
        with log_path.open("w") as log:
            client = Client(binary, fixture, root / "gtk.json", log)
            try:
                exercise(client, root / "gtk.json")
            except Exception:
                print(log_path.read_text(encoding="utf-8"), file=sys.stderr)
                raise
            finally:
                client.close()
    print("Passed: MCP, private sockets, PNG, pointer, click, scroll, keys, Unicode, stale frame, stop/restart, SIGTERM")


if __name__ == "__main__":
    main()
