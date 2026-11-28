# Installation options and app examples

## Download the Codex installer

On Linux x86_64 with Codex CLI installed, run the published installer as your regular user.

```sh
curl -fsSLo install-codex.sh https://raw.githubusercontent.com/nobadwordss/gnome-wayland-mcp/v0.2.8/scripts/install-codex.sh &&
  bash install-codex.sh
```

## Download the Linux x86_64 binary

The [v0.2.8 release](https://github.com/nobadwordss/gnome-wayland-mcp/releases/tag/v0.2.8) provides a statically linked Linux x86_64 executable. You can install and run it without a Rust toolchain.

```sh
archive=gnome-wayland-mcp-v0.2.8-linux-x86_64.tar.gz
release=https://github.com/nobadwordss/gnome-wayland-mcp/releases/download/v0.2.8
curl -fLO "$release/$archive" &&
  curl -fLO "$release/$archive.sha256" &&
  sha256sum -c "$archive.sha256" &&
  tar -xzf "$archive" &&
  install -Dm755 "${archive%.tar.gz}/gnome-wayland-mcp" "$HOME/.local/bin/gnome-wayland-mcp"
```

## Build from source

Use Rust 1.91 or later to build the server from a checkout.
Both the release binary and source builds use the [same runtime dependencies](../README.md#requirements).

```sh
git clone https://github.com/nobadwordss/gnome-wayland-mcp.git
cd gnome-wayland-mcp
cargo build --locked --release
```

## Connect your MCP client

Use a client that launches local stdio servers and passes image tool results to the model. The examples use the downloaded binary in `~/.local/bin`. For a source build, use the absolute path to `target/release/gnome-wayland-mcp`. For the Codex installer, use the binary path printed by `codex mcp get gnome-wayland-mcp`. The default monitor is 1280 × 800. Pass `--width` and `--height` before the server's `--` to change it.

### Codex

Use manual registration with a downloaded or locally built binary. The executable after the server's `--` is the app that `launch_app` will start. Use a separate entry name to keep multiple app configurations. If you used the installer, go directly to [Try it](../README.md#try-it).

```sh
codex mcp add gnome-wayland-mcp -- "$HOME/.local/bin/gnome-wayland-mcp" \
  -- /usr/bin/gnome-text-editor --standalone
codex mcp get gnome-wayland-mcp
```

If the tools are unavailable in a new chat, run `codex mcp get gnome-wayland-mcp` to check the registration and executable path. Codex may ask for approval before clicking, pasting, sending keys, or stopping the private desktop. Review the tool and its target before approving. If the server can open or capture the app but input is blocked, check the session's approval policy. A non-interactive `codex exec` session with `-a never` rejects tool calls that require approval. [Codex approval settings](https://learn.chatgpt.com/docs/config-file/config-reference)

### Claude Code

```sh
claude mcp add --transport stdio --scope user gnome-wayland-mcp \
  -- "$HOME/.local/bin/gnome-wayland-mcp" -- /usr/bin/gnome-text-editor --standalone
claude mcp get gnome-wayland-mcp
```

See the [Claude Code stdio setup guide](https://code.claude.com/docs/en/mcp#option-3-add-a-local-stdio-server).

### Gemini CLI

Add this entry under `mcpServers` in your user `~/.gemini/settings.json` and replace `/home/you` with your own absolute path.

```json
{
  "mcpServers": {
    "gnome-wayland-mcp": {
      "command": "/home/you/.local/bin/gnome-wayland-mcp",
      "args": ["--", "/usr/bin/gnome-text-editor", "--standalone"]
    }
  }
}
```

Run `gemini mcp list` to check the connection. See the [Gemini CLI MCP configuration guide](https://geminicli.com/docs/tools/mcp-server/).

## Optional Codex skill

After [installing for Codex](../README.md#install-for-codex) or [registering manually](#codex), you can add the [`$gnome-wayland-mcp` skill](../skills/gnome-wayland-mcp/SKILL.md) for reusable guidance on running apps in the private desktop and verifying mouse and keyboard input.

```sh
skill_file="$(mktemp)" &&
curl -fsSL https://raw.githubusercontent.com/nobadwordss/gnome-wayland-mcp/main/skills/gnome-wayland-mcp/SKILL.md \
  -o "$skill_file" && \
  install -Dm644 "$skill_file" "${CODEX_HOME:-$HOME/.codex}/skills/gnome-wayland-mcp/SKILL.md" &&
  rm -f "$skill_file"
```

Open a new Codex session and describe the app task in your own words. Codex can select the skill when the request matches its description. You can also mention `$gnome-wayland-mcp` to select it explicitly. [Codex skill invocation](https://learn.chatgpt.com/docs/build-skills#how-chatgpt-and-codex-use-skills)

> 내 화면과 마우스에 영향을 주지 않고 GNOME 텍스트 편집기를 열어 새 문서에 “봄바람 체크”와 “한글 입력 확인”을 한 줄씩 넣어줘. 앱 화면에서 두 줄을 확인한 뒤 저장 없이 테스트용 데스크톱을 종료하고 종료됐는지 확인해줘.

## Firefox example

With native Wayland Firefox installed at `/usr/bin/firefox`, add a separate MCP entry for it. If you built the server from source, use the absolute path to `target/release/gnome-wayland-mcp` instead of the installed binary path.

```sh
codex mcp add gnome-wayland-firefox -- "$HOME/.local/bin/gnome-wayland-mcp" \
  -- /usr/bin/firefox --no-remote --new-instance about:blank
```

The server gives Firefox a temporary home, and the Firefox flags start a separate instance. Open a new Codex session and try the following prompt.

> Use `$gnome-wayland-mcp` with the `gnome-wayland-firefox` MCP entry. Start the private desktop and launch Firefox. Capture the window and click the address bar. Enter `https://example.com/`, then capture the window to verify the address. Press Enter, capture the loaded page, and stop the private desktop.

## Ptyxis example

Use this example with GNOME's Ptyxis terminal installed at `/usr/bin/ptyxis`, Bash at `/usr/bin/bash`, and `gnome-wayland-mcp` v0.2.2 or later registered in Codex. Launch Codex with an argument override for this invocation. The temporary `PATH` lets Ptyxis start Bash directly inside the private session by omitting the optional `systemd-run` launcher.

```sh
(
  set -eu
  test_path=$(mktemp -d /tmp/gnome-wayland-ptyxis.XXXXXXXX)
  trap 'rm -f -- "$test_path/bash"; rmdir -- "$test_path"' EXIT
  ln -s /usr/bin/bash "$test_path/bash"

  ptyxis_args=$(printf '%s' \
    '["--", "/usr/bin/env", "PATH='"$test_path"'", "LC_ALL=C.UTF-8", ' \
    '"PS1=paste-test $ ", "/usr/bin/ptyxis", "--standalone", ' \
    '"--", "/usr/bin/bash", "--noprofile", "--norc", "-i"]')
  codex -c "mcp_servers.gnome-wayland-mcp.args=$ptyxis_args"
)
```

In that Codex session, use `$gnome-wayland-mcp` and follow these steps.

1. Call `desktop_start`, then `launch_app`. Capture the terminal and click its pane once the shell prompt appears.
2. Call `type_text` with `{"text":"한글 붙여넣기 확인 🌱🙂","paste_shortcut":"ctrl_shift_v"}`. Send one line with no trailing newline. Leave the text at the prompt without pressing Enter.
3. Take a fresh screenshot and verify the Korean text and emoji on the same prompt line. If they have yet to appear, capture again before retrying.
4. Call `desktop_stop` and confirm that `desktop_status` reports `stopped`. Exit the test Codex invocation to remove the temporary `PATH` directory.
