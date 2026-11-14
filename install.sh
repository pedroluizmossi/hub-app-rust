#!/bin/sh
# Installs (or updates) MiniUtils on Linux, from the latest Release on GitHub.
#
#   curl -fsSL https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.sh | sh
#
# For the current user only, no sudo:
#   ~/.local/bin/miniutils                            the program
#   ~/.local/share/applications/miniutils.desktop     entry in the applications menu
#   ~/.local/share/doc/miniutils/                     README and licenses
#
# Uninstall (the configuration in ~/.config/miniutils stays):
#   curl -fsSL https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.sh | sh -s -- --uninstall
#
# A specific version: MINIUTILS_VERSION=0.2.0 (instead of the latest).

set -eu

REPO="pedroluizmossi/hub-app-rust"
BIN_DIR="$HOME/.local/bin"
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}"
BIN="$BIN_DIR/miniutils"
DESKTOP="$DATA_DIR/applications/miniutils.desktop"
DOC_DIR="$DATA_DIR/doc/miniutils"
AUTOSTART="$CONFIG_DIR/autostart/miniutils.desktop"

say() { printf '%s\n' "$*"; }
fail() { printf 'erro: %s\n' "$*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || fail "the '$1' command is needed"; }

uninstall() {
    rm -f "$BIN" "$DESKTOP" "$AUTOSTART"
    rm -rf "$DOC_DIR"
    say "MiniUtils removed. The configuration in $CONFIG_DIR/miniutils was not deleted."
}

# Quoted path for the Exec line of the .desktop file (same rule as src/autostart/linux.rs).
quote_exec() {
    escaped=$(printf '%s' "$1" | sed -e 's/\\/\\\\\\\\/g' -e 's/["`$]/\\\\&/g' -e 's/%/%%/g')
    printf '"%s"' "$escaped"
}

install() {
    [ "$(uname -s)" = "Linux" ] || fail "this script is for Linux; on Windows use install.ps1 (see the README)"
    case "$(uname -m)" in
        x86_64 | amd64) ;;
        *) fail "there is only a package for x86_64 (this machine is $(uname -m))" ;;
    esac
    need curl
    need tar
    need sha256sum

    if [ -n "${MINIUTILS_VERSION:-}" ]; then
        version="${MINIUTILS_VERSION#v}"
    else
        # /releases/latest redirects to .../releases/tag/vX.Y.Z
        url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") ||
            fail "could not look up the latest Release"
        tag="${url##*/}"
        version="${tag#v}"
        case "$version" in
            [0-9]*) ;;
            *) fail "no Release published at github.com/$REPO" ;;
        esac
    fi

    name="miniutils-$version-linux-x86_64"
    archive="$name.tar.gz"
    base="https://github.com/$REPO/releases/download/v$version"

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "Downloading MiniUtils $version..."
    curl -fSL --progress-bar -o "$tmp/$archive" "$base/$archive" || fail "failed to download $base/$archive"
    curl -fsSL -o "$tmp/$archive.sha256" "$base/$archive.sha256" || fail "failed to download the SHA-256 sum"
    (cd "$tmp" && sha256sum -c --status "$archive.sha256") ||
        fail "the SHA-256 sum does not match: the downloaded file is corrupt or was altered"
    tar -xzf "$tmp/$archive" -C "$tmp"

    mkdir -p "$BIN_DIR" "$DATA_DIR/applications" "$DOC_DIR"
    # copy next to it and swap with mv: works even with MiniUtils open
    cp "$tmp/$name/miniutils" "$BIN.new"
    chmod 755 "$BIN.new"
    mv -f "$BIN.new" "$BIN"
    cp "$tmp/$name/README.md" "$tmp/$name/LICENSE" "$tmp/$name/THIRD_PARTY_NOTICES.md" "$DOC_DIR/"

    exec_line=$(quote_exec "$BIN")
    cat >"$DESKTOP" <<EOF
[Desktop Entry]
Type=Application
Name=MiniUtils
Comment=Mini-app hub in the tray
Comment[pt_BR]=Hub de mini-apps na bandeja
Exec=$exec_line
Terminal=false
Categories=Utility;
EOF
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$DATA_DIR/applications" >/dev/null 2>&1 || true
    fi

    say "MiniUtils $version installed in $BIN"
    say "Open it from the applications menu or run: miniutils"

    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) say "Note: $BIN_DIR is not in PATH. To run 'miniutils' in the terminal, add it to your shell's PATH." ;;
    esac
    if command -v pgrep >/dev/null 2>&1 && pgrep -x miniutils >/dev/null 2>&1; then
        say "MiniUtils is open: quit it (Quit, in the tray menu) and open it again to use the new version."
    fi
}

case "${1:-}" in
    --uninstall) uninstall ;;
    "") install ;;
    *) fail "unknown option: $1 (use --uninstall to remove)" ;;
esac
