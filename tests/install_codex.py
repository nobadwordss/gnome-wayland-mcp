"""Run the release installer with the real Codex CLI and disposable user configurations."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib


INSTALLER = Path(__file__).resolve().parents[1] / "scripts/install-codex.sh"
SERVER = "gnome-wayland-mcp"

NODE_REPL_TABLE = """[mcp_servers.node_repl]
# Keep the explicit defaults and the user's formatting.
command = '/usr/bin/true'
args = []
startup_timeout_sec = 120
"""
EXISTING_CONFIG = """# User settings
model = "gpt-6.2-sol"

""" + NODE_REPL_TABLE + """
[mcp_servers.example]
url = "https://example.invalid/mcp"
enabled = false

[tui]
status_line = ["current-dir"]
"""
INLINE_CONFIG = 'mcp_servers = { node_repl = { command = "/usr/bin/true", args = [], startup_timeout_sec = 120 } }\n'


def main():
    manifest = (INSTALLER.parents[1] / "Cargo.toml").read_text(encoding="utf-8")
    version = tomllib.loads(manifest)["package"]["version"]
    with tempfile.TemporaryDirectory(prefix="gwm-install-test-") as directory:
        root = Path(directory)
        app = root / "app with spaces"
        app.symlink_to("/usr/bin/true")
        custom = ["--", str(app), "한글 with spaces", "", "--option", '"quoted"\\path', "line 1\nline 2"]
        editor = "/usr/bin/gnome-text-editor"
        default = ["--", editor, "--standalone"] if os.access(editor, os.X_OK) else []

        for name, arguments, expected, existing in (
            ("default", [], default, ""),
            ("custom", custom, custom, EXISTING_CONFIG),
            ("desktop-only", ["--"], [], INLINE_CONFIG),
        ):
            profile = root / name
            codex_home = profile / (".codex" if name == "default" else "Codex settings")
            codex_home.mkdir(parents=True)
            config = codex_home / "config.toml"
            if existing:
                config.write_text(existing, encoding="utf-8")
            env = dict(os.environ, HOME=str(profile), CODEX_HOME=str(codex_home),
                       XDG_CONFIG_HOME=str(profile / ".config"), XDG_CACHE_HOME=str(profile / ".cache"),
                       XDG_DATA_HOME=str(profile / ".local/share"), XDG_STATE_HOME=str(profile / ".local/state"))
            if name == "default":
                env.pop("CODEX_HOME")
            installer = ["bash", INSTALLER]
            get = ["codex", "mcp", "get", "--json", SERVER]
            if name == "custom":
                invalid = subprocess.run([*installer, "--", "relative-app"], env=env, cwd=profile,
                                         capture_output=True, text=True)
                assert invalid.returncode != 0 and "absolute path" in invalid.stderr, invalid
                assert config.read_text(encoding="utf-8") == existing
            subprocess.run([*installer, *arguments], env=env, cwd=profile, check=True)
            entry = json.loads(subprocess.check_output(get, env=env, cwd=profile))["transport"]
            binary = Path(entry["command"])
            assert binary == profile / ".local/share" / SERVER / f"v{version}" / SERVER, binary
            assert subprocess.check_output([binary, "--version"], text=True).strip() == f"{SERVER} {version}"
            assert entry["args"] == expected, entry
            installed = config.read_text(encoding="utf-8")
            settings = tomllib.loads(installed)
            settings["mcp_servers"].pop(SERVER)
            if not settings["mcp_servers"]:
                del settings["mcp_servers"]
            # Unlike Python's numeric equality, JSON distinguishes 120 from 120.0.
            assert json.dumps(settings, sort_keys=True) == json.dumps(tomllib.loads(existing), sort_keys=True), settings
            if name == "custom":
                assert NODE_REPL_TABLE in installed
                subprocess.run([*installer, "--"], env=env, cwd=profile, check=True)
                assert config.read_text(encoding="utf-8") == installed
                assert json.loads(subprocess.check_output(get, env=env, cwd=profile))["transport"] == entry
            print(f"Passed {name} installer registration with app arguments {expected!r}", flush=True)


if __name__ == "__main__":
    main()
