# Hello, external app (MiniUtils example)

An example of an **external app**: the logic is an executable of its own (`src/main.rs`, in
Rust), which the hub starts when the app is turned on and talks to in JSON, one message per
line, on standard input and output. The view is `view.slint`, as in a plugin with a view.

- **Full access:** it runs with your user's permissions. This example reads the home folder
  and writes the counter to `state.json`, in the data folder the hub gives it
  (`MINIUTILS_APP_DATA`).
- It reads the `sys` provider (CPU) and gets busy above 80%.
- **Translation:** texts are written in English. [`i18n/pt-BR.po`](i18n/pt-BR.po) translates
  the manifest and the view's `@tr("…")` (the hub does that), and the messages the app sends
  itself: `start` brings `lang`, and the app reads the same file (it runs in its own folder).
- Manifest format: top of `src/core/plugins.rs`. Protocol: top of `src/apps/external.rs`.

## Develop

```sh
cargo build -p app-hello
mkdir -p examples/app-hello/bin
ln -sf ../../../target/debug/app-hello examples/app-hello/bin/app-hello
cargo run -- --plugin-dev examples/app-hello
```

In dev mode the link in `bin/` works, and each `cargo build -p app-hello` restarts the app in
the hub. What the app writes with `eprintln!` shows up in the hub's terminal. On Windows,
copy `target\debug\app-hello.exe` to `bin\app-hello.exe` (creating a link needs privileges).

## Publish in the store

The executable does not go in the repository: the store index gives, per OS, the address of
the binary and its SHA-256, and announces `"full_access": true`:

```json
{
  "id": "io.github.pedroluizmossi.app-hello",
  "full_access": true,
  "uses": ["sys"],
  "binaries": {
    "linux": { "url": "https://…/app-hello", "sha256": "…" },
    "windows": { "url": "https://…/app-hello.exe", "sha256": "…" }
  }
}
```

Installation downloads the binary for this OS, checks the hash and shows the full-access
warning before installing.

App types and when to use each one: [APPS.md](../../APPS.md).
