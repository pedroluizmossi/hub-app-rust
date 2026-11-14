#!/bin/sh
# Builds an example store to test plugins without internet:
#   1. a git repository (the "community repo") with the plugins from examples/, each in a
#      subfolder: plugin-uptime (simple), plugin-top (its own view and script) and
#      app-hello (external app, with full access);
#   2. a registry.json (the "index") pinning each plugin's commit and, for the external
#      app, the executable built here (outside the repository) with its SHA-256;
#   3. an ed25519 key and the index signature (registry.json.sig).
#
# Usage:
#   examples/store/prepare.sh [folder]                 builds everything (default: /tmp/miniutils-store)
#   examples/store/prepare.sh --new-version [folder]   publishes Uptime 0.2.0 (every 10 s and
#                                                      also asking for `who`), to test the
#                                                      update and the new-permission warning
# At the end it shows the index and the public key to put in the Store (Settings > Store).
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
bin=${MINIUTILS_BIN:-"cargo run -q --manifest-path $here/../Cargo.toml --"}

new=false
if [ "${1:-}" = "--new-version" ] || [ "${1:-}" = "--nova-versao" ]; then
    new=true
    shift
fi
dest=${1:-/tmp/miniutils-store}
repo="$dest/example-plugins"

commit_all() {
    git -C "$repo" add .
    git -C "$repo" -c user.name=example -c user.email=example@example.com commit --quiet -m "$1"
}

if $new; then
    [ -d "$repo/.git" ] || { echo "run it first without --new-version" >&2; exit 1; }
    sed -i 's/^every = 30$/every = 10/; s/^exec = \["uptime"\]$/exec = ["uptime", "who"]/' "$repo/uptime/manifest.toml"
    commit_all "Uptime 0.2.0"
    uptime_version=0.2.0
    uptime_perms='"uptime", "who"'
else
    rm -rf "$dest"
    mkdir -p "$repo/uptime/i18n" "$repo/top/i18n" "$repo/hello/i18n"
    for f in manifest.toml README.md LICENSE i18n/pt-BR.po; do cp "$here/plugin-uptime/$f" "$repo/uptime/$f"; done
    for f in manifest.toml view.slint main.rhai README.md LICENSE i18n/pt-BR.po; do cp "$here/plugin-top/$f" "$repo/top/$f"; done
    for f in manifest.toml view.slint README.md LICENSE i18n/pt-BR.po; do cp "$here/app-hello/$f" "$repo/hello/$f"; done
    git -C "$repo" init --quiet
    commit_all "example plugins"
    uptime_version=0.1.0
    uptime_perms='"uptime"'
    # the store maintainer's key (the private one stays in $dest/key, owner-only)
    $bin --store-key "$dest/key" > "$dest/key.pub"
fi
commit=$(git -C "$repo" rev-parse HEAD)
top_commit=$(git -C "$repo" log -n 1 --format=%H -- top)
hello_commit=$(git -C "$repo" log -n 1 --format=%H -- hello)

# the external app's executable: built here, published outside the repository (in a real
# store, the store's CI builds it from the reviewed commit and publishes it in a release)
cargo build -q --release -p app-hello --manifest-path "$here/../Cargo.toml"
cp "$here/../target/release/app-hello" "$dest/app-hello"
hello_sha=$(sha256sum "$dest/app-hello" | cut -d' ' -f1)

cat > "$dest/registry.json" <<JSON
{
  "schema": 1,
  "plugins": [
    {
      "id": "io.github.exemplo.uptime",
      "name": "Uptime",
      "description": "How long the computer has been on (uptime -p). A simple plugin.",
      "author": "example",
      "license": "MIT",
      "version": "$uptime_version",
      "repo": "$repo",
      "path": "uptime",
      "commit": "$commit",
      "permissions": [$uptime_perms]
    },
    {
      "id": "io.github.pedroluizmossi.top-processes",
      "name": "Top processes",
      "description": "The processes using the most CPU or memory, with its own view and script.",
      "author": "example",
      "license": "MIT",
      "version": "0.1.0",
      "repo": "$repo",
      "path": "top",
      "commit": "$top_commit",
      "uses": ["procs"]
    },
    {
      "id": "io.github.pedroluizmossi.app-hello",
      "name": "Hello, external app",
      "description": "Example external app: an executable of its own, with full access.",
      "author": "example",
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
$bin --store-sign "$dest/registry.json" "$dest/key" > /dev/null
echo "index:       $dest/registry.json"
echo "public key:  $(cat "$dest/key.pub")"
