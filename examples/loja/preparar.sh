#!/bin/sh
# Monta uma loja de exemplo para testar plugins sem internet:
#   1. um repositório git (o "repo da comunidade") com os plugins de examples/, cada um
#      numa subpasta: plugin-uptime (simples), plugin-top (tela e script próprios) e
#      app-hello (app externo, com acesso total);
#   2. um registry.json (o "índice") fixando o commit de cada plugin e, para o app externo,
#      o executável compilado aqui (fora do repositório) com o SHA-256 dele;
#   3. uma chave ed25519 e a assinatura do índice (registry.json.sig).
#
# Uso:
#   examples/loja/preparar.sh [pasta]               monta tudo (padrão: /tmp/miniutils-loja)
#   examples/loja/preparar.sh --nova-versao [pasta] publica o Tempo ligado 0.2.0 (a cada 10 s
#                                                    e pedindo também o `who`), para testar
#                                                    a atualização e o aviso de permissão nova
# No fim, mostra o índice e a chave pública para pôr na Loja (Configurações > Loja).
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
bin=${MINIUTILS_BIN:-"cargo run -q --manifest-path $here/../Cargo.toml --"}

nova=false
if [ "${1:-}" = "--nova-versao" ]; then
    nova=true
    shift
fi
dest=${1:-/tmp/miniutils-loja}
repo="$dest/plugins-exemplo"

commit_all() {
    git -C "$repo" add .
    git -C "$repo" -c user.name=exemplo -c user.email=exemplo@example.com commit --quiet -m "$1"
}

if $nova; then
    [ -d "$repo/.git" ] || { echo "rode primeiro sem --nova-versao" >&2; exit 1; }
    sed -i 's/^every = 30$/every = 10/; s/^exec = \["uptime"\]$/exec = ["uptime", "who"]/' "$repo/uptime/manifest.toml"
    commit_all "Tempo ligado 0.2.0"
    uptime_version=0.2.0
    uptime_perms='"uptime", "who"'
else
    rm -rf "$dest"
    mkdir -p "$repo/uptime" "$repo/top" "$repo/hello"
    for f in manifest.toml README.md LICENSE; do cp "$here/plugin-uptime/$f" "$repo/uptime/"; done
    for f in manifest.toml view.slint main.rhai README.md LICENSE; do cp "$here/plugin-top/$f" "$repo/top/"; done
    for f in manifest.toml view.slint README.md LICENSE; do cp "$here/app-hello/$f" "$repo/hello/"; done
    git -C "$repo" init --quiet
    commit_all "plugins de exemplo"
    uptime_version=0.1.0
    uptime_perms='"uptime"'
    # chave de quem mantém a loja (a privada fica em $dest/chave, só para o dono)
    $bin --loja-chave "$dest/chave" > "$dest/chave.pub"
fi
commit=$(git -C "$repo" rev-parse HEAD)
top_commit=$(git -C "$repo" log -n 1 --format=%H -- top)
hello_commit=$(git -C "$repo" log -n 1 --format=%H -- hello)

# o executável do app externo: compilado aqui, publicado fora do repositório (numa loja de
# verdade, o CI da loja compila a partir do commit revisado e publica num release)
cargo build -q --release -p app-hello --manifest-path "$here/../Cargo.toml"
cp "$here/../target/release/app-hello" "$dest/app-hello"
hello_sha=$(sha256sum "$dest/app-hello" | cut -d' ' -f1)

cat > "$dest/registry.json" <<JSON
{
  "schema": 1,
  "plugins": [
    {
      "id": "io.github.exemplo.uptime",
      "name": "Tempo ligado",
      "description": "Há quanto tempo o computador está ligado (uptime -p). Plugin simples.",
      "author": "exemplo",
      "license": "MIT",
      "version": "$uptime_version",
      "repo": "$repo",
      "path": "uptime",
      "commit": "$commit",
      "permissions": [$uptime_perms]
    },
    {
      "id": "io.github.exemplo.top",
      "name": "Top processos",
      "description": "Os processos que mais usam CPU ou memória, com tela e script próprios.",
      "author": "exemplo",
      "license": "MIT",
      "version": "0.1.0",
      "repo": "$repo",
      "path": "top",
      "commit": "$top_commit",
      "permissions": ["ps"]
    },
    {
      "id": "io.github.pedroluizmossi.app-hello",
      "name": "Olá, app externo",
      "description": "App externo de exemplo: um executável próprio, com acesso total.",
      "author": "exemplo",
      "license": "MIT",
      "version": "0.1.0",
      "repo": "$repo",
      "path": "hello",
      "commit": "$hello_commit",
      "uses": ["sys"],
      "full_access": true,
      "binaries": {
        "linux": { "url": "$dest/app-hello", "sha256": "$hello_sha" }
      }
    }
  ]
}
JSON
$bin --loja-assinar "$dest/registry.json" "$dest/chave" > /dev/null
echo "índice:         $dest/registry.json"
echo "chave pública:  $(cat "$dest/chave.pub")"
