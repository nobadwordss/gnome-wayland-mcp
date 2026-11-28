#!/usr/bin/env bash
set -euo pipefail

server=gnome-wayland-mcp
version=0.2.8
archive=$server-v$version-linux-x86_64.tar.gz
release=https://github.com/nobadwordss/gnome-wayland-mcp/releases/download/v$version
requirements=https://github.com/nobadwordss/gnome-wayland-mcp#requirements

fail() {
    printf 'Error: %s\n' "$*" >&2
    exit 1
}

skip_if_registered() {
    if codex mcp get "$server" >/dev/null 2>&1; then
        printf '%s is already registered. To inspect it, run: codex mcp get %s\n' "$server" "$server"
        exit 0
    fi
}

if [[ ${1-} == --help || ${1-} == -h ]]; then
    printf '%s\n' 'Usage: install-codex.sh [-- /absolute/path/to/app [ARG...]]' \
        'With no arguments, use GNOME Text Editor if installed. Use -- alone for a desktop without an app.'
    exit 0
fi

[[ $(uname -sm) == 'Linux x86_64' ]] || fail 'This installer requires Linux x86_64.'
command -v codex >/dev/null || fail 'Install the Codex CLI or put it on PATH first.'

codex mcp list --json >/dev/null || fail 'Could not read the Codex MCP configuration.'
skip_if_registered

if (($# == 0)) && [[ -x /usr/bin/gnome-text-editor ]]; then
    set -- /usr/bin/gnome-text-editor --standalone
elif [[ ${1-} == -- ]]; then
    shift
fi
app_args=()
if (($#)); then
    [[ $1 == /* && -f $1 && -x $1 ]] || fail "Expected an absolute path to an executable app, got '$1'."
    app_args=(-- "$@")
fi

missing=()
for command in curl python3 sha256sum tar; do
    command -v "$command" >/dev/null || missing+=("$command")
done
for executable in dbus-daemon pipewire pw-dump wireplumber mutter gst-launch-1.0 gst-inspect-1.0; do
    [[ -x /usr/bin/$executable ]] || missing+=("/usr/bin/$executable")
done
if ((${#missing[@]})); then
    printf 'Missing required commands:\n' >&2
    printf '  %s\n' "${missing[@]}" >&2
    fail "See the runtime requirements: $requirements"
fi

wireplumber_version=$(/usr/bin/wireplumber --version 2>&1) || fail 'Could not check the WirePlumber version.'
if [[ ! $wireplumber_version =~ Linked\ with\ libwireplumber[[:blank:]]+([0-9]+)\.([0-9]+) ]] ||
    ((10#${BASH_REMATCH[1]} == 0 && 10#${BASH_REMATCH[2]} < 5)); then
    fail "WirePlumber 0.5 or later is required. See $requirements"
fi

install_dir=$HOME/.local/share/$server/v$version
mkdir -p -- "$install_dir"
stage=$(mktemp -d "$install_dir/.install.XXXXXXXX")
trap 'rm -rf -- "$stage"' EXIT

inspect=(env -i PATH=/usr/bin HOME="$stage" LC_ALL=C
    GST_REGISTRY="$stage/gstreamer-registry.bin" /usr/bin/gst-inspect-1.0)
for element in queue videorate videoconvert pngenc fdsink; do
    "${inspect[@]}" --exists "$element" ||
        fail "Missing GStreamer element '$element'. See $requirements"
done
pipewiresrc_info=$("${inspect[@]}" pipewiresrc) ||
    fail "Missing GStreamer element 'pipewiresrc'. See $requirements"
[[ $pipewiresrc_info == *GST_PIPEWIRE_SRC_ON_DISCONNECT_ERROR* ]] ||
    fail "GStreamer pipewiresrc must support on-disconnect=error. See $requirements"

binary=$install_dir/$server
(
    cd -- "$stage"
    for asset in "$archive" "$archive.sha256"; do
        curl -q --fail --silent --show-error --location --retry 3 \
            --connect-timeout 10 --max-time 60 --retry-max-time 90 \
            --proto '=https' --proto-redir '=https' --output "$asset" "$release/$asset" ||
            fail "Could not download $asset from GitHub."
    done

    if ! read -r checksum filename extra < "$archive.sha256" ||
        [[ ! $checksum =~ ^[[:xdigit:]]{64}$ || -n $extra || ${filename#\*} != "$archive" ]]; then
        fail "Unexpected checksum file for $archive."
    fi
    printf '%s  %s\n' "$checksum" "$archive" | sha256sum --check --strict - ||
        fail 'The downloaded archive failed checksum verification.'

    tar -xOzf "$archive" -- "${archive%.tar.gz}/$server" > "$server" ||
        fail 'Could not extract the MCP server from the release archive.'
    chmod 0755 "$server"
    [[ $(./"$server" --version) == "$server $version" ]] ||
        fail "The extracted MCP server did not report version $version."
    mv -fT -- "$server" "$binary"
)

skip_if_registered
# Write one entry through Codex's config API. The CLI's mcp add command
# serializes every server and can discard explicit defaults in other entries.
if ! python3 - "$server" "$binary" "${app_args[@]}" <<'PY'
import json
import signal
import subprocess
import sys

name, binary, *args = sys.argv[1:]


def register(client):
    def request(method, params=None, *, notify=False):
        message = {"method": method}
        if params is not None:
            message["params"] = params
        if not notify:
            message["id"] = method
        client.stdin.write(json.dumps(message) + "\n")
        client.stdin.flush()
        if not notify:
            for line in client.stdout:
                reply = json.loads(line)
                if reply.get("id") == method:
                    if "error" in reply:
                        raise RuntimeError(f"{method}: {reply['error']['message']}")
                    return reply["result"]
            raise RuntimeError(f"Codex closed before replying to {method}")

    request("initialize", {"clientInfo": {"name": name + "-installer", "version": "1"}})
    request("initialized", notify=True)
    snapshot = request("config/read", {"includeLayers": True})
    user = next((layer for layer in snapshot["layers"] if layer["name"]["type"] == "user"), None)
    if user is None:
        raise RuntimeError("Codex did not return a user configuration layer")
    if any(name in (config.get("mcp_servers") or {}) for config in (user["config"], snapshot["config"])):
        print(f"{name} is already registered.")
        return
    result = request("config/value/write", {
        "keyPath": f"mcp_servers.{name}",
        "mergeStrategy": "replace",
        "value": {"command": binary, "args": args},
        "expectedVersion": user["version"],
    })
    if result["status"] != "ok":
        raise RuntimeError("The MCP registration was overridden by another Codex config layer")


def timed_out(*_):
    raise TimeoutError("Timed out waiting for the Codex config API")


try:
    with subprocess.Popen(["codex", "app-server"], stdin=subprocess.PIPE,
                          stdout=subprocess.PIPE, text=True, encoding="utf-8") as client:
        signal.signal(signal.SIGALRM, timed_out)
        signal.alarm(30)
        try:
            register(client)
        finally:
            signal.alarm(0)
            client.stdin.close()
            try:
                client.wait(timeout=5)
            except subprocess.TimeoutExpired:
                client.kill()
except (OSError, RuntimeError, ValueError, KeyError) as error:
    print(f"Codex config update failed ({error})", file=sys.stderr)
    sys.exit(1)
PY
then
    fail "Could not register $server with Codex. The verified binary is at $binary."
fi
codex mcp get "$server" || fail "Could not verify the Codex registration for $server."
if ((${#app_args[@]} == 0)); then
    printf '\nNo app configured. Use desktop_start, screenshot and desktop_stop to try the private display.\n'
fi
printf '\nOpen a new Codex chat and follow the first-run example in the README.\n'
