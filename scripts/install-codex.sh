#!/usr/bin/env bash
set -euo pipefail

server=gnome-wayland-mcp
version=0.2.6
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

[[ $(uname -sm) == 'Linux x86_64' ]] || fail 'This installer requires Linux x86_64.'
command -v codex >/dev/null || fail 'Install the Codex CLI or put it on PATH first.'

codex mcp list --json >/dev/null || fail 'Could not read the Codex MCP configuration.'
skip_if_registered

missing=()
for command in curl sha256sum tar; do
    command -v "$command" >/dev/null || missing+=("$command")
done
for executable in dbus-daemon pipewire wireplumber mutter gst-launch-1.0 gst-inspect-1.0 gnome-text-editor; do
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

for element in pipewiresrc queue videorate videoconvert pngenc fdsink; do
    env -i PATH=/usr/bin HOME="$stage" GST_REGISTRY="$stage/gstreamer-registry.bin" \
        /usr/bin/gst-inspect-1.0 --exists "$element" ||
        fail "Missing GStreamer element '$element'. See $requirements"
done

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
codex mcp add "$server" -- "$binary" -- /usr/bin/gnome-text-editor --standalone ||
    fail "Could not register $server with Codex. The verified binary is at $binary."
codex mcp get "$server" || fail "Could not verify the Codex registration for $server."
printf '\nOpen a new Codex chat and follow the first-run example in the README.\n'
