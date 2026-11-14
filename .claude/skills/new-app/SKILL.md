---
name: new-app
description: Creates a new MiniUtils app (simple plugin, plugin with a view, external app or built-in mini-app) from the project's examples, and only hands it over after seeing it work on screen. Use when the user asks for a new app, plugin, widget, card or mini-app for the hub, even without saying which type.
---

# New MiniUtils app

The reference guide is `APPS.md` (types, manifest, providers, script and protocol API).
This skill says **where to start, what to copy and how to check it**; don't repeat what is
there, read it.

## 1. Pick the type

Use "Qual escolher" in `APPS.md`: the simplest type that does the job.

| Request | Type | Copy from |
|---|---|---|
| show the output of a command | simple plugin | `examples/plugin-uptime` |
| its own view, buttons, provider data, HTTPS | plugin with a view | `examples/plugin-top` |
| the same, keeping state or history in a database | plugin with a view + `[data] kinds = ["sqlite"]` | `examples/plugin-notes` |
| files, sockets, local network, long-running process, another language | external app | `examples/app-hello` |
| part of the product, new provider, compiled view | built-in mini-app | `miniapps/todo` + `src/apps/todo.rs` |

If the request fits two types, or you don't know where to create the folder, ask **one**
question only (type and folder together). Plugins and external apps are repositories of
their own: suggest `../miniutils-plugin-<name>` (or `../miniutils-app-<name>`), outside this
repository. A built-in one lives here.

**For personal use only, not published** (e.g. someone who installed MiniUtils with
`install.sh` and doesn't have this repository): create the plugin directly in the apps
folder, which MiniUtils reads by itself. The path comes from `miniutils --dirs` (the local
apps line); the app's folder must be named exactly as its `id`. See `APPS.md › Apps locais`.

## 2. Create

**Plugin or external app**
1. Copy the whole example folder (manifest, `README.md`, `LICENSE`, `i18n/`, view, script or
   `src/`). An external app outside this repository is a crate on its own: the `bin/` link
   points to its own `target/` (`ln -sf ../target/debug/<name> bin/<name>`), not to this
   repository's as in the `app-hello` README.
2. `id = "io.github.<owner>.<name>"` (owner: the one from this repository's `git remote`,
   unless the user says otherwise). `cyber.*` is reserved for built-in apps.
3. `icon`: an existing mascot (`key:` in `src/mascot/sprites.rs`). Read the list; don't
   make up a name.
4. `uses` and `[permissions]` only with what the app really uses (the store shows it on
   install). What must last between runs goes in `[data]` (`kv` for loose values, `sqlite`
   for tables; an external app writes to its `data_dir` folder), never in a loose file or
   in `this`. Database: create the tables in the current format with
   `CREATE TABLE IF NOT EXISTS` in `start()`; if the format changes after publishing, bump
   `version` and write `fn migrate(from, to)`. See `APPS.md › Dados que duram`.
5. View: `component Main`, colors from `theme`, `font-size: 13px * theme.text-scale`,
   `overflow: elide` on text, and hide the secondary parts in a small cell (~300 × 160 px).
   See "A tela" in `APPS.md`.
6. Texts: write them in English with `@tr("…")` in the view and in English in the manifest
   (`name`, `description`, `label`), and put the Brazilian Portuguese in `i18n/pt-BR.po`
   (`msgid` English, `msgstr` translation), as the examples do. A script has no catalog:
   send numbers and let the view phrase them (`examples/plugin-notes`). An external app
   gets `lang` in `start` and translates its own messages.

**Built-in mini-app**: follow `DESIGN.md › 5. Mini-apps`, with To-do as the model. The
places a new app touches (check with `grep -rn todo` in case something moved):
- `miniapps/<app>/manifest.toml` (`id = "cyber.<name>"`, a `theme` option with `default =
  "auto"` and no `options`: the hub fills in the themes) and `miniapps/<app>/view.slint`
  (`export global <App>App` + `export component <App>View`; `font-size` always
  `× Skin.text-scale`);
- texts in English: `@tr("…")` in the view, `tr!("…")` in Rust, `name`/`description`/`label`
  in the manifest, and the pt-BR translations in `i18n/pt-BR/miniapps.po` (the test
  `catalog_covers_every_string` points out what is missing);
- `src/apps/<app>.rs` (a struct with `create(host, ui)` + the `MiniApp` trait) and `pub mod`
  in `src/apps/mod.rs`; if it stores something, `[data] kinds = ["files"]` in the manifest
  and the file in `appdata::dir(&manifest.id)` (like `todo.json`), plus `reload_data` in
  the trait for Settings' "Delete data";
- `src/hub/mod.rs`: `registry.add(crate::apps::<app>::<App>Mini::create(host.clone(), &ui) as AppRef, None);`
  next to the others;
- `ui/dashboard.slint`: import the view and add `if c.id == "cyber.<name>": <App>View { … }`;
- `ui/hub.slint`: import the global and the view's structs and put them in `export { … }`;
- `src/core/manifest.rs`: the new manifest in the list of the test
  `built_in_manifests_are_valid`.

## 3. Check it (don't hand over without this)

`tools/dev-ui.sh` opens an isolated MiniUtils (it doesn't touch the user's instance or
config) and lets you **see** the view. See `AGENTS.md › Interface (Slint)`.

**Plugin or external app**
```sh
tools/dev-ui.sh open -- --plugin-dev <folder>   # starts turned on, on the 1st page
tools/dev-ui.sh shot                            # then read the PNG with Read
tools/dev-ui.sh log                             # view and script errors, the app's stderr
```
Dev mode reloads on save: edit, wait ~1 s and take another `shot`. Click the view's buttons
(`tools/dev-ui.sh find <text>`, `click <text>`) and check the reaction.

Without this repository (no `tools/dev-ui.sh`), with the app in the apps folder: run
`miniutils --reload` and check in `miniutils --plugins` that it isn't among the skipped
ones; ask the user to turn it on in Settings and look at the view.

**Built-in mini-app**
1. `cargo check`, then `cargo test` and `cargo clippy --workspace --all-targets -- -D warnings`.
2. `tools/dev-ui.sh open`, put the app on a page (Settings › apps, with `click`; or edit
   `target/dev-ui/config/miniutils/config.toml` and open again) and take a `shot`.
3. Visual tweaks in a row: `tools/dev-ui.sh open --live`, edit `view.slint`,
   `tools/dev-ui.sh reload`, `shot` (each round takes seconds, without recompiling).

## 4. Hand over

Say which type you picked and why, the files created, what was checked (with the
screenshot) and what wasn't (e.g. Windows, which can only be checked on a real Windows).
To publish, point to "Publicar" in `APPS.md`; don't open a pull request on the store unless
the user asks.
