# Top processos (plugin de exemplo do MiniUtils)

Os processos que mais usam CPU ou memória, lidos com `ps` a cada 5 segundos. Exemplo de
plugin **com tela e script próprios**: `view.slint` (a tela) e `main.rhai` (a lógica).

- Permissões: executa só o programa `ps`.
- O robô fica ocupado quando um processo passa de 80% de CPU.
