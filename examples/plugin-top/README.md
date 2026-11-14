# Top processes (MiniUtils example plugin)

The processes using the most CPU or memory, the same on Linux and Windows. An example of a
plugin **with its own view and script**: `view.slint` (the view) and `main.rhai` (the
logic).

- Permissions: runs nothing. The processes come from the hub's `procs` provider
  (`uses = ["procs"]`), in `fn on_event`.
- The robot gets busy when a process goes over 50% CPU.
- Translation: the view's texts are English `@tr("…")`; [`i18n/pt-BR.po`](i18n/pt-BR.po)
  translates them and the manifest's name and description.
- App types and what each one can do: [APPS.md](../../APPS.md).
