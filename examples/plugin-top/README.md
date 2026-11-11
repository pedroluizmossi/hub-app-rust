# Top processos (plugin de exemplo do MiniUtils)

Os processos que mais usam CPU ou memória, iguais no Linux e no Windows. Exemplo de plugin
**com tela e script próprios**: `view.slint` (a tela) e `main.rhai` (a lógica).

- Permissões: não executa nada. Os processos chegam do provedor `procs` do hub
  (`uses = ["procs"]`), em `fn on_event`.
- O robô fica ocupado quando um processo passa de 50% de CPU.
- Os tipos de app e o que cada um pode: [APPS.md](../../APPS.md).
