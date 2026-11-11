#!/bin/sh
# Instala (ou atualiza) o MiniUtils no Linux, a partir do último Release no GitHub.
#
#   curl -fsSL https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.sh | sh
#
# Só para o usuário, sem sudo:
#   ~/.local/bin/miniutils                            o programa
#   ~/.local/share/applications/miniutils.desktop     atalho no menu de aplicativos
#   ~/.local/share/doc/miniutils/                     README e licenças
#
# Desinstalar (a configuração em ~/.config/miniutils fica):
#   curl -fsSL https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.sh | sh -s -- --uninstall
#
# Uma versão específica: MINIUTILS_VERSION=0.2.0 (em vez da última).

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
need() { command -v "$1" >/dev/null 2>&1 || fail "precisa do comando '$1'"; }

uninstall() {
    rm -f "$BIN" "$DESKTOP" "$AUTOSTART"
    rm -rf "$DOC_DIR"
    say "MiniUtils removido. A configuração em $CONFIG_DIR/miniutils não foi apagada."
}

# Caminho entre aspas para a linha Exec do .desktop (mesma regra de src/autostart/linux.rs).
quote_exec() {
    escaped=$(printf '%s' "$1" | sed -e 's/\\/\\\\\\\\/g' -e 's/["`$]/\\\\&/g' -e 's/%/%%/g')
    printf '"%s"' "$escaped"
}

install() {
    [ "$(uname -s)" = "Linux" ] || fail "este script é para Linux; no Windows use o install.ps1 (ver o README)"
    case "$(uname -m)" in
        x86_64 | amd64) ;;
        *) fail "só há pacote para x86_64 (esta máquina é $(uname -m))" ;;
    esac
    need curl
    need tar
    need sha256sum

    if [ -n "${MINIUTILS_VERSION:-}" ]; then
        version="${MINIUTILS_VERSION#v}"
    else
        # /releases/latest redireciona para .../releases/tag/vX.Y.Z
        url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") ||
            fail "não consegui consultar o último Release"
        tag="${url##*/}"
        version="${tag#v}"
        case "$version" in
            [0-9]*) ;;
            *) fail "nenhum Release publicado em github.com/$REPO" ;;
        esac
    fi

    name="miniutils-$version-linux-x86_64"
    archive="$name.tar.gz"
    base="https://github.com/$REPO/releases/download/v$version"

    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT INT TERM

    say "Baixando MiniUtils $version..."
    curl -fSL --progress-bar -o "$tmp/$archive" "$base/$archive" || fail "falha ao baixar $base/$archive"
    curl -fsSL -o "$tmp/$archive.sha256" "$base/$archive.sha256" || fail "falha ao baixar a soma SHA-256"
    (cd "$tmp" && sha256sum -c --status "$archive.sha256") ||
        fail "a soma SHA-256 não bate: o arquivo baixado está corrompido ou foi alterado"
    tar -xzf "$tmp/$archive" -C "$tmp"

    mkdir -p "$BIN_DIR" "$DATA_DIR/applications" "$DOC_DIR"
    # copia ao lado e troca com mv: funciona mesmo com o MiniUtils aberto
    cp "$tmp/$name/miniutils" "$BIN.new"
    chmod 755 "$BIN.new"
    mv -f "$BIN.new" "$BIN"
    cp "$tmp/$name/README.md" "$tmp/$name/LICENSE" "$tmp/$name/THIRD_PARTY_NOTICES.md" "$DOC_DIR/"

    exec_line=$(quote_exec "$BIN")
    cat >"$DESKTOP" <<EOF
[Desktop Entry]
Type=Application
Name=MiniUtils
Comment=Hub de mini-apps na bandeja
Exec=$exec_line
Terminal=false
Categories=Utility;
EOF
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "$DATA_DIR/applications" >/dev/null 2>&1 || true
    fi

    say "MiniUtils $version instalado em $BIN"
    say "Abra pelo menu de aplicativos ou rode: miniutils"

    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) say "Aviso: $BIN_DIR não está no PATH. Para rodar 'miniutils' no terminal, adicione-o ao PATH do seu shell." ;;
    esac
    if command -v pgrep >/dev/null 2>&1 && pgrep -x miniutils >/dev/null 2>&1; then
        say "O MiniUtils está aberto: feche-o (Sair, no menu da bandeja) e abra de novo para usar a versão nova."
    fi
}

case "${1:-}" in
    --uninstall) uninstall ;;
    "") install ;;
    *) fail "opção desconhecida: $1 (use --uninstall para remover)" ;;
esac
