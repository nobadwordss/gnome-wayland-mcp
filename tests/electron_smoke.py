"""Exercise an Electron renderer through the private Wayland desktop and MCP."""

import argparse
from contextlib import ExitStack
import os
from pathlib import Path
import select
import sys
import tempfile

from PIL import Image, ImageChops

from headless_smoke import Client, initialize, point, poll, runtime_processes, snapshot


def center_of_color(image, color):
    red, green, blue = ImageChops.difference(image, Image.new("RGB", image.size, color)).split()
    distance = ImageChops.lighter(ImageChops.lighter(red, green), blue)
    # Capture can shift the rendered colors slightly across compositor profiles.
    box = distance.point(lambda value: 255 if value <= 12 else 0).getbbox()
    if box is None:
        return None
    left, top, right, bottom = box
    if right - left >= 100 and bottom - top >= 24:
        return (left + right) // 2, (top + bottom) // 2
    return None


def targets(image):
    return {"button": center_of_color(image, "#d84292"), "editor": center_of_color(image, "#187b73")}


def click_target(client, name):
    frame = poll(lambda: client.call("screenshot", inspect_frame=targets),
                 lambda frame: frame["button"] is not None and frame["editor"] is not None,
                 "screenshot contains the Electron button and editor")
    client.call("mouse_click", {"point": point(frame, *frame[name])})


def process_group_members(leader):
    members = set()
    for process in Path("/proc").iterdir():
        if process.name.isdigit():
            pid = int(process.name)
            try:
                if os.getpgid(pid) == leader:
                    members.add(pid)
            except (PermissionError, ProcessLookupError):
                continue
    return members


def verify_shutdown(client, runtime, app):
    browser = app["pid"]
    renderer = app["renderer"]
    assert os.getpgid(browser) == browser and renderer > 0 and renderer != browser
    # Chromium subprocesses may omit environment entries in /proc. Track the
    # managed application group and Electron's own process IDs as well.
    processes = runtime_processes(runtime) | process_group_members(browser) | set(app["processes"]) | {renderer}
    exited = select.poll()
    with ExitStack() as handles:
        pending = set()
        for pid in processes:
            try:
                handle = os.pidfd_open(pid)
            except ProcessLookupError:
                continue
            handles.callback(os.close, handle)
            pending.add(handle)
            exited.register(handle, select.POLLIN)
        assert client.call("desktop_stop")["status"] == "stopped"
        poll(lambda: (runtime.exists(), runtime_processes(runtime), {fd for fd, _ in exited.poll(0)}),
             lambda state: state == (False, set(), pending),
             "private runtime and Electron processes to exit")
    assert client.call("desktop_status")["status"] == "stopped"


def exercise(client, report):
    initialize(client)
    started = client.call("desktop_start")
    assert started["status"] == "desktop_running" and started["mode"] == "headless"
    assert client.call("launch_app")["status"] == "application_running"

    def read_state():
        state = snapshot(report)
        if not state:
            status = client.call("desktop_status")["status"]
            assert status == "application_running", f"Electron exited before renderer readiness ({status})"
        return state

    app = poll(read_state, lambda state: state.get("renderer"), "Electron renderer ready")
    runtime = Path(app["runtime"])
    assert app["ozone"] == app["sessionType"] == "wayland"
    assert app["display"] is None and app["waylandSocket"] is None
    assert app["wayland"] == "mcp-wayland" and (runtime / app["wayland"]).is_socket()
    assert Path(app["profile"]).is_relative_to(runtime)

    click_target(client, "button")
    poll(read_state, lambda state: state["clicks"] == 1, "trusted click near the window title bar")
    click_target(client, "editor")
    poll(read_state, lambda state: state["focused"], "textarea gets keyboard focus after its click")

    expected = ""
    for count, text in enumerate(("ASCII ", "한글 ", "🌱🙂\ncafé"), 1):
        client.call("type_text", {"text": text})
        expected += text
        poll(read_state, lambda state: state["text"] == expected and state["pastes"] == count,
             "Chromium gets exactly one paste event and the full text")

    client.call("press_key", {"key": "a", "modifiers": ["ctrl"]})
    poll(read_state, lambda state: state["selected"] == expected, "Ctrl+A selects the pasted text")
    client.call("press_key", {"key": "Backspace"})
    poll(read_state, lambda state: state["text"] == "", "Backspace clears the selection")
    client.call("type_text", {"text": "다시 붙여넣기 🚀"})
    app = poll(read_state, lambda state: state["text"] == "다시 붙여넣기 🚀" and state["pastes"] == 4,
               "replacement paste reaches the focused textarea")
    verify_shutdown(client, runtime, app)
    print(f"Passed Electron {app['electron']}: native Wayland frame, clicks, focus, Unicode paste, shortcuts and cleanup")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("server", type=Path)
    parser.add_argument("electron", type=Path)
    args = parser.parse_args()
    fixture = Path(__file__).with_name("electron_fixture.cjs").resolve()
    with tempfile.TemporaryDirectory(prefix="gwm-electron-smoke-") as directory:
        root = Path(directory)
        report = root / "electron.json"
        log_path = root / "mcp.log"
        command = [args.electron.resolve(), "--ozone-platform=wayland", fixture, report]
        with log_path.open("w") as log:
            client = Client(args.server.resolve(), command, log)
            try:
                exercise(client, report)
            except Exception:
                print(log_path.read_text(encoding="utf-8"), file=sys.stderr)
                raise
            finally:
                client.close()


if __name__ == "__main__":
    main()
