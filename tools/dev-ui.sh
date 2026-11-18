#!/usr/bin/env bash
# Opens a development MiniUtils and drives it through Slint's built-in MCP server (feature
# `dev-mcp`, see Cargo.toml). It lets an agent (Claude Code, Codex) see the interface: PNG
# screenshots, finding elements, clicking. Linux only; needs curl and jq.
# See AGENTS.md › "Interface (Slint)".
#
#   tools/dev-ui.sh open [--live] [--my-config] [-- app args]
#       builds and opens an isolated instance (closes the previous one). --live: reloads
#       the .slint files on save (feature live-preview). --my-config: starts from your
#       real config.
#   tools/dev-ui.sh reload                (--live) reloads the .slint files now; see below
#   tools/dev-ui.sh shot [file.png]       screenshot of the window (default: target/dev-ui/screen.png)
#                                         DEV_UI_WINDOW=1: of the 2nd window (terminal, details…)
#   tools/dev-ui.sh find <text>           elements whose label or id contains the text
#   tools/dev-ui.sh click <text> [n]      clicks the n-th element found (default: the 1st)
#   tools/dev-ui.sh mcp [tool [json]]     any MCP tool (without a name: lists them)
#   tools/dev-ui.sh log                   the app's output (Slint and live-preview errors)
#   tools/dev-ui.sh close                 ends the instance started by `open`
#
# The instance uses MINIUTILS_INSTANCE=dev-ui (it does not talk to the MiniUtils you use)
# and XDG_CONFIG_HOME/XDG_STATE_HOME in target/dev-ui (it does not touch your config or
# your logs). The installed plugins are your real ones; their data is not:
# MINIUTILS_DATA=target/dev-ui/app-data (starts empty and leaves yours alone).
#
# Limits of Slint 1.18's live-preview (verified): (1) it only notices a file written in
# place (VS Code, `cat >`); writing by replacing the file (the agent's Edit tool, `sed -i`,
# vim) goes unnoticed: after editing, run `reload`. (2) After a reload, `find`/`click`
# still see the old tree; `shot` shows the new one. To click on the new screen, run
# `open --live` again. (3) The window goes back to its minimum width on reload.
set -euo pipefail
cd "$(dirname "$0")/.."
DIR=target/dev-ui
PORT=${SLINT_MCP_PORT:-9315}
URL="http://127.0.0.1:$PORT/mcp"

die() { echo "dev-ui: $*" >&2; exit 1; }

rpc() { # <method> <params json>
    curl -sS -m 30 "$URL" -H 'Content-Type: application/json' \
        -d "$(jq -nc --arg m "$1" --argjson p "$2" '{jsonrpc: "2.0", id: 1, method: $m, params: $p}')" \
        || die "no answer at $URL (is the app open? run: tools/dev-ui.sh open)"
}

tool() { # <tool> [args json] → the whole answer; an MCP error becomes a script error
    local args=${2:-'{}'} out
    out=$(rpc tools/call "$(jq -nc --arg n "$1" --argjson a "$args" '{name: $n, arguments: $a}')")
    if jq -e '.error or .result.isError' <<<"$out" >/dev/null; then
        die "$1: $(jq -r '.error.message // .result.content[0].text' <<<"$out")"
    fi
    printf '%s' "$out"
}

text() { jq -r '.result.content[] | select(.type == "text") | .text'; }

# The open window (the main one, or the n-th with DEV_UI_WINDOW=n, counting from 0: a
# window the app opens besides the hub). Its handle changes on every reload: always ask.
window() { tool list_windows | text | jq -c --argjson n "${DEV_UI_WINDOW:-0}" '.windowHandles[$n] // empty'; }

# elements of the window whose accessible label or id contains the text (case-insensitive)
find_elements() {
    local w root
    w=$(window); [ -n "$w" ] || die "no window open"
    root=$(tool get_window_properties "{\"windowHandle\": $w}" | text | jq -c .rootElementHandle)
    tool get_element_tree "{\"elementHandle\": $root, \"maxElements\": 1000}" | text \
        | jq -c --arg q "$1" '.elements[]
            | select(([.accessibleLabel // ""] + [.typeNamesAndIds[]?.id // empty])
                     | any(ascii_downcase | contains($q | ascii_downcase)))'
}

running_pid() { # pid of the instance started by `open`, if it is still up
    local pid
    pid=$(cat "$DIR/pid" 2>/dev/null) || return 1
    grep -q miniutils "/proc/$pid/cmdline" 2>/dev/null && echo "$pid"
}

close() {
    local pid
    if pid=$(running_pid); then
        kill "$pid"
        for _ in $(seq 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
    fi
    rm -f "$DIR/pid"
}

cmd=${1:-}; shift || true
case "$cmd" in
open)
    features=dev-mcp mine=
    while [ $# -gt 0 ]; do
        case "$1" in
            --live) features=dev-mcp,live-preview ;;
            --my-config) mine=1 ;;
            --) shift; break ;;
            *) die "open: unknown option $1" ;;
        esac
        shift
    done
    close
    cargo build --features "$features"
    mkdir -p "$DIR/config/miniutils" "$DIR/state"
    if [ -n "$mine" ]; then
        cp "${XDG_CONFIG_HOME:-$HOME/.config}/miniutils/config.toml" "$DIR/config/miniutils/config.toml"
    fi
    SLINT_MCP_PORT=$PORT MINIUTILS_INSTANCE=dev-ui \
        XDG_CONFIG_HOME="$PWD/$DIR/config" XDG_STATE_HOME="$PWD/$DIR/state" \
        MINIUTILS_DATA="$PWD/$DIR/app-data" \
        setsid target/debug/miniutils "$@" >"$DIR/app.log" 2>&1 </dev/null &
    echo $! >"$DIR/pid"
    for _ in $(seq 60); do
        grep -q "MCP server listening" "$DIR/app.log" && { echo "open (pid $(cat "$DIR/pid"), $features)"; exit 0; }
        running_pid >/dev/null || { tail -20 "$DIR/app.log" >&2; die "the app exited while opening"; }
        sleep 0.5
    done
    tail -20 "$DIR/app.log" >&2
    die "MCP did not answer within 30 s"
    ;;
reload)
    # rewrites hub.slint in the same file (same content): live-preview notices and rereads
    # every .slint, including the ones the Edit tool replaced
    tmp=$(mktemp)
    cat ui/hub.slint >"$tmp" && cat "$tmp" >ui/hub.slint
    rm -f "$tmp"
    sleep 2 # live-preview waits a little and recompiles the interface
    grep "Reloaded\|error" "$DIR/app.log" | tail -3
    ;;
shot|screenshot)
    out=${1:-$DIR/screen.png}
    w=$(window); [ -n "$w" ] || die "no window open"
    tool take_screenshot "{\"windowHandle\": $w}" \
        | jq -r '.result.content[] | select(.type == "image") | .data' | base64 -d >"$out"
    echo "$out"
    ;;
find)
    [ $# -ge 1 ] || die "find <text>"
    find_elements "$1" | jq -r '[
            (.accessibleLabel // "-"),
            ([.typeNamesAndIds[] | .typeName + (if .id then "#" + .id else "" end)] | join(" < ")),
            "\(.absolutePosition.x // 0),\(.absolutePosition.y // 0) \(.size.width // 0)x\(.size.height // 0)",
            (.accessibleRole // "")
        ] | join(" | ")' | nl -w2 -s') '
    ;;
click)
    [ $# -ge 1 ] || die "click <text> [n]"
    handle=$(find_elements "$1" | sed -n "${2:-1}p" | jq -c .handle)
    [ -n "$handle" ] || die "no element with \"$1\" (see: tools/dev-ui.sh find \"$1\")"
    tool click_element "{\"elementHandle\": $handle}" >/dev/null
    echo "clicked"
    ;;
mcp)
    if [ $# -eq 0 ]; then
        rpc tools/list '{}' | jq -r '.result.tools[] | "\(.name): \(.description)"'
    else
        tool "$1" "${2:-}" | text
    fi
    ;;
log) cat "$DIR/app.log" ;;
close) close; echo "closed" ;;
*) sed -n '2,31p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
