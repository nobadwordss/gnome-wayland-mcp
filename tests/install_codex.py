"""Run the release installer with the real Codex CLI and disposable user configurations."""

import json
import os
from pathlib import Path
import subprocess
import tempfile


INSTALLER = Path(__file__).resolve().parents[1] / "scripts/install-codex.sh"
SERVER = "gnome-wayland-mcp"


def main():
    with tempfile.TemporaryDirectory(prefix="gwm-install-test-") as directory:
        root = Path(directory)
        app = root / "app with spaces"
        app.symlink_to("/usr/bin/true")
        custom = ["--", str(app), "한글 with spaces", "", "--option"]
        editor = "/usr/bin/gnome-text-editor"
        default = ["--", editor, "--standalone"] if os.access(editor, os.X_OK) else []

        for name, arguments, expected in (
            ("default", [], default), ("custom", custom, custom), ("desktop-only", ["--"], []),
        ):
            profile = root / name
            (profile / ".codex").mkdir(parents=True)
            env = dict(os.environ, HOME=str(profile), CODEX_HOME=str(profile / ".codex"),
                       XDG_CONFIG_HOME=str(profile / ".config"), XDG_CACHE_HOME=str(profile / ".cache"),
                       XDG_DATA_HOME=str(profile / ".local/share"), XDG_STATE_HOME=str(profile / ".local/state"))
            installer = ["bash", INSTALLER]
            get = ["codex", "mcp", "get", "--json", SERVER]
            if name == "custom":
                invalid = subprocess.run([*installer, "--", "relative-app"], env=env, cwd=profile,
                                         capture_output=True, text=True)
                assert invalid.returncode != 0 and "absolute path" in invalid.stderr, invalid
            subprocess.run([*installer, *arguments], env=env, cwd=profile, check=True)
            entry = json.loads(subprocess.check_output(get, env=env, cwd=profile))["transport"]
            binary = Path(entry["command"])
            assert binary.is_relative_to(profile) and os.access(binary, os.X_OK), binary
            assert entry["args"] == expected, entry
            if name == "custom":
                subprocess.run([*installer, "--"], env=env, cwd=profile, check=True)
                assert json.loads(subprocess.check_output(get, env=env, cwd=profile))["transport"] == entry
            print(f"Passed {name} installer registration with app arguments {expected!r}", flush=True)


if __name__ == "__main__":
    main()
