"""Check Codex tool selection and completion with a delayed Wayland window."""

import argparse
import base64
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import sys
import tempfile
from threading import Timer

from PIL import Image, ImageChops

from headless_smoke import runtime_processes, snapshot


SKILL = Path(__file__).resolve().parents[1] / "skills/gnome-wayland-mcp/SKILL.md"
FIXTURE = Path(__file__).resolve().with_name("gtk_fixture.py")
SERVER = "gnome-wayland-mcp"
BODY = "봄바람 체크 7412\n한글 두 줄 확인"
FILE_LINE = "커피 한 잔"
GUI_PROMPT = (
    '별도 화면에서 GTK 텍스트 편집기를 열어 첫 줄에 "봄바람 체크 7412", '
    '둘째 줄에 "한글 두 줄 확인"을 입력해줘. 입력한 두 줄이 앱 화면에 '
    '실제로 보이는지 확인하고, 앱에 표시된 화면 확인 코드도 알려줘. '
    '저장 없이 테스트용 데스크톱을 종료하고 종료됐는지 확인해줘.'
)
FILE_PROMPT = "이 폴더의 sample.txt를 읽고 두 번째 줄만 그대로 알려줘."
STAGE_COLOR = (38, 64, 90)
EDITOR = (455, 112, 845, 205)
INPUT_TOOLS = {"mouse_click", "mouse_move", "mouse_scroll", "press_key", "type_text"}


def run_codex(args, directory, prompt, on_event):
    workdir = directory / "work"
    workdir.mkdir(parents=True)
    skill_path = workdir / ".agents/skills/gnome-wayland-mcp/SKILL.md"
    if not args.without_skill:
        skill_path.parent.mkdir(parents=True)
        shutil.copyfile(SKILL, skill_path)
    (workdir / "sample.txt").write_text(f"첫째 줄\n{FILE_LINE}\n세 번째 줄\n", encoding="utf-8")
    (directory / "prompt.txt").write_text(prompt + "\n", encoding="utf-8")

    xdg = directory / "xdg"
    xdg.mkdir(mode=0o700)
    env = os.environ.copy()
    for key in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DBUS_SESSION_BUS_ADDRESS"):
        env.pop(key, None)
    env["XDG_RUNTIME_DIR"] = str(xdg)

    server = f"mcp_servers.{SERVER}"
    app = ["--width", "960", "--height", "540", "--", sys.executable, str(FIXTURE),
           str(directory / "gtk.json"), str(directory / "show-window")]
    skill_enabled = str(not args.without_skill).lower()
    final = directory / "final.txt"
    command = [
        "codex", "-a", "never", "exec", "--ephemeral", "--ignore-user-config",
        "--skip-git-repo-check", "--json", "--color", "never", "--sandbox", "read-only",
        "-C", str(workdir), "-c", f"{server}.required=true",
        "-c", f"{server}.command={json.dumps(str(args.binary))}",
        "-c", f"{server}.args={json.dumps(app)}",
        "-c", f'skills.config=[{{name="{SERVER}",enabled={skill_enabled}}}]',
        "-o", str(final),
    ]
    for tool in ("mouse_click", "type_text", "press_key", "desktop_stop"):
        command.extend(["-c", f'{server}.tools.{tool}.approval_mode="approve"'])
    if args.model:
        command.extend(["--model", args.model])
    if args.reasoning_effort:
        command.extend(["-c", f"model_reasoning_effort={json.dumps(args.reasoning_effort)}"])

    with (directory / "events.jsonl").open("w", encoding="utf-8") as log, \
            (directory / "stderr.log").open("w", encoding="utf-8") as stderr:
        with subprocess.Popen([*command, prompt], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                              stderr=stderr, text=True, encoding="utf-8", env=env) as child:
            deadline = Timer(args.timeout, child.kill)
            deadline.start()
            try:
                for line in child.stdout:
                    log.write(line)
                    log.flush()
                    event = json.loads(line)
                    item = event.get("item", {})
                    if item.get("type") == "mcp_tool_call" and item.get("server") == SERVER:
                        on_event(event["type"], item)
                assert child.wait() == 0, f"Codex timed out or failed. See {directory}"
            except BaseException:
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        child.kill()
                raise
            finally:
                deadline.cancel()
    return final.read_text(encoding="utf-8").strip()


def verify_gui(args, directory):
    report = directory / "gtk.json"
    code = str(100000 + secrets.randbelow(900000))
    gate = directory / "show-window"
    completed = []
    frame_count = 0
    window_visible = False
    before_input = None
    pasted = False
    evidence = None

    def inspect_event(kind, item):
        nonlocal frame_count, window_visible, before_input, pasted, evidence
        tool = item["tool"]
        if kind == "item.started" and tool in INPUT_TOOLS:
            assert window_visible, f"{tool} attempted before a screenshot showed the app"
        if kind != "item.completed":
            return
        assert item["status"] == "completed" and not item.get("error"), (tool, item.get("error"))
        result = item["result"]["content"]
        info = json.loads(next(entry["text"] for entry in result if entry["type"] == "text"))
        completed.append((tool, info))
        if tool == "type_text":
            assert info["status"] == "dispatched", info
            pasted = True
        if tool == "desktop_stop":
            assert evidence is not None, "Desktop stopped before a screenshot showed the paste"
        if tool != "screenshot":
            return

        entry = next(entry for entry in result if entry["type"] == "image")
        path = directory / f"frame-{frame_count:02d}.png"
        frame_count += 1
        path.write_bytes(base64.b64decode(entry["data"], validate=True))
        with Image.open(path) as image:
            assert image.format == "PNG" and image.size == (960, 540), path
            pixels = image.convert("RGB")
        window_visible = pixels.getpixel((30, 30)) == STAGE_COLOR
        if not gate.exists() and any(name == "launch_app" for name, _ in completed):
            assert not window_visible and not report.exists(), "First capture after launch must precede the window"
            temporary = gate.with_suffix(".tmp")
            temporary.write_text(code, encoding="utf-8")
            temporary.replace(gate)
        if not window_visible:
            return
        crop = pixels.crop(EDITOR)
        if not pasted:
            before_input = crop
        elif snapshot(report).get("text") == BODY and before_input is not None:
            if ImageChops.difference(before_input, crop).getbbox():
                evidence = path

    answer = run_codex(args, directory, GUI_PROMPT, inspect_event)
    tools = [tool for tool, _ in completed]
    assert "desktop_start" in tools and "launch_app" in tools, tools
    assert "type_text" in tools and "desktop_stop" in tools, tools
    assert any(tool == "desktop_status" and info["status"] == "stopped"
               for tool, info in completed[tools.index("desktop_stop") + 1:]), tools
    assert completed[tools.index("desktop_stop")][1]["status"] == "stopped", completed
    state = snapshot(report)
    assert state.get("text") == BODY, f"GTK buffer did not receive the exact Korean text. See {report}"
    assert re.search(rf"(?<!\d){code}(?!\d)", answer), f"Codex did not report the screen code. See {directory}"
    runtime = Path(state["runtime"])
    assert not runtime.exists() and not runtime_processes(runtime), f"Desktop resources remain: {runtime}"
    print(f"PASS GUI: waited for the app, verified exact Korean text and screen code, stopped. {evidence}")


def verify_file(args, directory):
    calls = set()
    answer = run_codex(args, directory, FILE_PROMPT, lambda _, item: calls.add(item["tool"]))
    assert not calls, f"File-only request called GUI tools: {sorted(calls)}"
    assert answer == FILE_LINE, f"Wrong file-only response. See {directory / 'final.txt'}"
    print("PASS file-only: exact second line, zero MCP calls.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=lambda value: Path(value).resolve(strict=True))
    parser.add_argument("--model")
    parser.add_argument("--reasoning-effort")
    parser.add_argument("--without-skill", action="store_true")
    parser.add_argument("--timeout", type=int, default=240, help="seconds per Codex run")
    args = parser.parse_args()
    directory = Path(tempfile.mkdtemp(prefix="gwm-codex-routing-"))
    print(f"Artifacts: {directory}", flush=True)
    verify_gui(args, directory / "gui")
    verify_file(args, directory / "file")


if __name__ == "__main__":
    main()
