# Notes (MiniUtils example plugin)

Quick notes kept in the **app's own SQLite database**. An example of a plugin with a view,
a script and data that lasts:

- `[data] kinds = ["sqlite"]` in the manifest: the hub opens `data.sqlite` in the app's data
  folder (outside the plugin folder, so it survives updates). The script uses `db_batch`,
  `db_exec` and `db_query`, with `?` parameters.
- `version = 3` and `fn migrate(from, to)`: data from version 1 gets the `done` column, and
  older data gets the table and columns renamed to English, before the app starts. If the
  migration fails, the hub puts back the copy from before.
- `max_mb = 5`: the database does not grow past that (SQLite refuses to write more).
- Translation: the view writes its texts in English with `@tr("…")`, and
  [`i18n/pt-BR.po`](i18n/pt-BR.po) translates them and the manifest's name and description.
  The script sends numbers (`todo`, `done`, each note's `age`) and lets the view phrase them.
- Permissions: runs nothing and does not use the network.

To try it: `miniutils --plugin-dev examples/plugin-notes`. The data shows up in
Settings › (app options) › Data, with Export and Delete. App types and what each one can
do: [APPS.md](../../APPS.md).
