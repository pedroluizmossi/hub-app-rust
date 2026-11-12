#!/usr/bin/env bash
# Abre um MiniUtils de desenvolvimento e o controla pelo servidor MCP embutido do Slint
# (recurso `dev-mcp`, ver Cargo.toml). Serve para um agente (Claude Code, Codex) ver a
# interface: captura em PNG, achar elementos, clicar. Só Linux; precisa de curl e jq.
# Ver AGENTS.md › "Ver e mexer na interface".
#
#   tools/dev-ui.sh abrir [--live] [--minha-config] [-- args do app]
#       compila e abre uma instância isolada (fecha a anterior). --live: recarrega os
#       .slint ao salvar (recurso live-preview). --minha-config: começa da sua config real.
#   tools/dev-ui.sh recarregar            (--live) recarrega os .slint agora; ver abaixo
#   tools/dev-ui.sh foto [arquivo.png]    captura da janela (padrão: target/dev-ui/tela.png)
#   tools/dev-ui.sh achar <texto>         elementos cujo rótulo ou id contém o texto
#   tools/dev-ui.sh clicar <texto> [n]    clica no n-ésimo elemento achado (padrão: 1º)
#   tools/dev-ui.sh mcp [ferramenta [json]]  qualquer ferramenta do MCP (sem nome: lista)
#   tools/dev-ui.sh log                   saída do app (erros do Slint, do live-preview)
#   tools/dev-ui.sh fechar                encerra a instância aberta por `abrir`
#
# A instância usa MINIUTILS_INSTANCE=dev-ui (não conversa com o MiniUtils que você usa) e
# XDG_CONFIG_HOME/XDG_STATE_HOME em target/dev-ui (não mexe na sua config nem no seu
# registro). Os plugins instalados são os seus, de verdade; os dados deles não:
# MINIUTILS_DADOS=target/dev-ui/app-data (começam vazios e não mexem nos seus).
#
# Limites do live-preview do Slint 1.18 (conferidos): (1) ele só percebe arquivo gravado
# no lugar (VS Code, `cat >`); gravar trocando o arquivo (a ferramenta Edit do agente,
# `sed -i`, vim) passa batido: depois de editar, rode `recarregar`. (2) Depois de uma
# recarga, `achar`/`clicar` ainda veem a árvore antiga; a `foto` mostra a nova. Para clicar
# na tela nova, `abrir --live` de novo. (3) A janela volta à largura mínima ao recarregar.
set -euo pipefail
cd "$(dirname "$0")/.."
DIR=target/dev-ui
PORT=${SLINT_MCP_PORT:-9315}
URL="http://127.0.0.1:$PORT/mcp"

die() { echo "dev-ui: $*" >&2; exit 1; }

rpc() { # <método> <params json>
    curl -sS -m 30 "$URL" -H 'Content-Type: application/json' \
        -d "$(jq -nc --arg m "$1" --argjson p "$2" '{jsonrpc: "2.0", id: 1, method: $m, params: $p}')" \
        || die "sem resposta em $URL (o app está aberto? rode: tools/dev-ui.sh abrir)"
}

tool() { # <ferramenta> [args json] → resposta inteira; erro do MCP vira erro do script
    local args=${2:-'{}'} out
    out=$(rpc tools/call "$(jq -nc --arg n "$1" --argjson a "$args" '{name: $n, arguments: $a}')")
    if jq -e '.error or .result.isError' <<<"$out" >/dev/null; then
        die "$1: $(jq -r '.error.message // .result.content[0].text' <<<"$out")"
    fi
    printf '%s' "$out"
}

text() { jq -r '.result.content[] | select(.type == "text") | .text'; }

# 1ª janela aberta (a principal). O identificador muda a cada recarga: pergunte sempre.
window() { tool list_windows | text | jq -c '.windowHandles[0] // empty'; }

# elementos da janela cujo rótulo acessível ou id contém o texto (sem diferenciar maiúsculas)
find_elements() {
    local w root
    w=$(window); [ -n "$w" ] || die "nenhuma janela aberta"
    root=$(tool get_window_properties "{\"windowHandle\": $w}" | text | jq -c .rootElementHandle)
    tool get_element_tree "{\"elementHandle\": $root, \"maxElements\": 1000}" | text \
        | jq -c --arg q "$1" '.elements[]
            | select(([.accessibleLabel // ""] + [.typeNamesAndIds[]?.id // empty])
                     | any(ascii_downcase | contains($q | ascii_downcase)))'
}

running_pid() { # pid da instância aberta por `abrir`, se ainda estiver de pé
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
abrir)
    features=dev-mcp mine=
    while [ $# -gt 0 ]; do
        case "$1" in
            --live) features=dev-mcp,live-preview ;;
            --minha-config) mine=1 ;;
            --) shift; break ;;
            *) die "abrir: opção desconhecida $1" ;;
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
        MINIUTILS_DADOS="$PWD/$DIR/app-data" \
        setsid target/debug/miniutils "$@" >"$DIR/app.log" 2>&1 </dev/null &
    echo $! >"$DIR/pid"
    for _ in $(seq 60); do
        grep -q "MCP server listening" "$DIR/app.log" && { echo "aberto (pid $(cat "$DIR/pid"), $features)"; exit 0; }
        running_pid >/dev/null || { tail -20 "$DIR/app.log" >&2; die "o app saiu ao abrir"; }
        sleep 0.5
    done
    tail -20 "$DIR/app.log" >&2
    die "o MCP não respondeu em 30 s"
    ;;
recarregar)
    # regrava o hub.slint no mesmo arquivo (conteúdo igual): o live-preview percebe e relê
    # todos os .slint, inclusive os que a Edit trocou
    tmp=$(mktemp)
    cat ui/hub.slint >"$tmp" && cat "$tmp" >ui/hub.slint
    rm -f "$tmp"
    sleep 2 # o live-preview espera um pouco e recompila a interface
    grep "Reloaded\|error" "$DIR/app.log" | tail -3
    ;;
foto)
    out=${1:-$DIR/tela.png}
    w=$(window); [ -n "$w" ] || die "nenhuma janela aberta"
    tool take_screenshot "{\"windowHandle\": $w}" \
        | jq -r '.result.content[] | select(.type == "image") | .data' | base64 -d >"$out"
    echo "$out"
    ;;
achar)
    [ $# -ge 1 ] || die "achar <texto>"
    find_elements "$1" | jq -r '[
            (.accessibleLabel // "-"),
            ([.typeNamesAndIds[] | .typeName + (if .id then "#" + .id else "" end)] | join(" < ")),
            "\(.absolutePosition.x // 0),\(.absolutePosition.y // 0) \(.size.width // 0)x\(.size.height // 0)",
            (.accessibleRole // "")
        ] | join(" | ")' | nl -w2 -s') '
    ;;
clicar)
    [ $# -ge 1 ] || die "clicar <texto> [n]"
    handle=$(find_elements "$1" | sed -n "${2:-1}p" | jq -c .handle)
    [ -n "$handle" ] || die "nenhum elemento com \"$1\" (veja: tools/dev-ui.sh achar \"$1\")"
    tool click_element "{\"elementHandle\": $handle}" >/dev/null
    echo "clicado"
    ;;
mcp)
    if [ $# -eq 0 ]; then
        rpc tools/list '{}' | jq -r '.result.tools[] | "\(.name): \(.description)"'
    else
        tool "$1" "${2:-}" | text
    fi
    ;;
log) cat "$DIR/app.log" ;;
fechar) close; echo "fechado" ;;
*) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
