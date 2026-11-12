# Notas (plugin de exemplo do MiniUtils)

Notas rápidas guardadas num **banco SQLite do app**. Exemplo de plugin com tela, script e
dados que duram:

- `[data] kinds = ["sqlite"]` no manifesto: o hub abre `data.sqlite` na pasta de dados do
  app (fora da pasta do plugin, então sobrevive a atualizar). O script usa `db_batch`,
  `db_exec` e `db_query`, com parâmetros `?`.
- `version = 2` e `fn migrate(de, para)`: quem tinha a versão 1 ganha a coluna `feita`
  antes de o app começar. Se a migração der erro, o hub devolve a cópia de antes.
- `max_mb = 5`: o banco não passa disso (o SQLite recusa gravar além).
- Permissões: não executa nada nem usa a rede.

Para testar: `miniutils --plugin-dev examples/plugin-notas`. Os dados aparecem em
Configurações › (opções do app) › Dados, com Exportar e Apagar. Os tipos de app e o que
cada um pode: [APPS.md](../../APPS.md).
