# Criar apps para o MiniUtils

O MiniUtils aceita quatro tipos de app. Todos aparecem do mesmo jeito para quem usa: uma
célula na grade do painel, uma mascote que reage ao estado do app, opções em
**Configurações** e lugar na conversa entre mascotes. O que muda é onde a lógica roda e
o que ela pode fazer.

| Tipo | Seção no manifesto | A lógica é | Pode | Distribuição | Exemplo |
|---|---|---|---|---|---|
| **Plugin simples** | `[run]` | um comando que o hub roda a cada N s | só os programas declarados | loja | [`examples/plugin-uptime`](examples/plugin-uptime) |
| **Plugin com tela** | `[ui]` | um script Rhai isolado + `view.slint` | API do hub: programas declarados, HTTPS para domínios declarados, provedores, dados (chave-valor e banco SQLite) | loja | [`examples/plugin-top`](examples/plugin-top), [`examples/plugin-notas`](examples/plugin-notas) (banco) |
| **App externo** | `[app]` | um executável seu (qualquer linguagem) + `view.slint` | **tudo** o que o usuário pode (acesso total) | loja, com binário conferido por SHA-256 | [`examples/app-hello`](examples/app-hello) |
| **Mini-app embutido** | (nenhuma) | Rust compilado dentro do hub | tudo, inclusive a interface compilada e provedores novos | pull request neste repositório | [`miniapps/todo`](miniapps/todo) + [`src/apps/todo.rs`](src/apps/todo.rs) |

## Qual escolher

Comece pelo mais simples que resolve:

1. **Mostrar a saída de um comando** (`uptime`, `df -h /`, `git -C ~/x status -s`)? Plugin
   simples. Só manifesto, sem código.
2. **Tela própria, botões, juntar dados dos provedores do hub, consultar uma API HTTPS,
   guardar estado ou histórico num banco**? Plugin com tela. O script é isolado: não lê
   arquivo nem roda nada fora do que o manifesto declara (o banco é do app, na pasta que o
   hub cuida), e a loja mostra exatamente isso antes de instalar.
3. **Ler ou gravar arquivos, vigiar pastas, sockets, `http://` ou rede local, bibliotecas
   nativas, processo de vida longa**? App externo. Ele roda com as permissões do usuário, e
   a loja o instala com um aviso de "ACESSO TOTAL" em destaque. Use só quando o script não
   alcança.
4. **Faz parte do produto** (todo mundo usaria), precisa de um provedor novo compartilhado
   ou da interface compilada? Mini-app embutido, por pull request.

## O que todos têm em comum

**Manifesto** (`manifest.toml`):

```toml
id = "io.github.fulano.meu-app"   # único; cyber.* é reservado aos apps embutidos
name = "Meu app"
description = "Uma frase sobre o que ele faz."
icon = "coruja"                   # mascote: lista em src/mascot/sprites.rs
api = 1
uses = ["sys"]                    # provedores do hub (abaixo); [ui] e [app]

[[settings]]                      # vira formulário em Configurações
key = "limite"
label = "Mascote agitada a partir de (%)"   # " (%)" ou " (°C)" no fim viram a unidade
type = "int"                      # bool, int ou choice
default = 80
min = 10
max = 100
step = 5
```

Plugins e apps externos ganham sozinhos as opções `theme` (tema de cores) e `mascot`.

**Provedores** (`uses`): o hub coleta cada dado uma vez e entrega a quem pediu, só enquanto
algum app ligado usa aquele provedor.

| Nome | Dados |
|---|---|
| `sys` | CPU (uso, frequência, temperatura, carga), memória e swap, rede, discos |
| `gpu` | NVIDIA (`nvidia-smi`) ou AMD (sysfs) |
| `docker` | containers, stacks, uso de cada um |
| `media` | o que está tocando (MPRIS no Linux, controles de mídia no Windows) |
| `ai_usage` | uso das assinaturas de IA (Codex, Claude, Kiro, OpenCode Go) |
| `ports` | portas escutando e o processo de cada uma |
| `agents` | sessões do Claude Code, Codex e OpenCode e o estado delas |
| `procs` | processos mais pesados por CPU e por memória (igual no Linux e no Windows) |

Os campos de cada um estão nos `struct …Snapshot` de `src/providers/`.

**Humor da mascote**: `sleep` < `idle` < `happy` < `busy` < `alarm`. A bandeja mostra o app
em pior humor, então use `busy` e `alarm` só quando o usuário deve olhar.

**Conversa entre mascotes**: o app conta fatos sobre si (`trouble`, `strain`, `forecast`,
`recovered`, `poked`) e o hub decide quem fala e quando. As mudanças de humor e as cutucadas
na mascote o hub já conta sozinho.

## 1. Plugin simples (`[run]`)

O hub roda um comando a cada `every` segundos e mostra a saída numa tela genérica.

```toml
[permissions]
exec = ["uptime"]            # só estes programas, chamados sem shell

[run]
command = ["uptime", "-p"]
every = 30                   # 5 a 3600 s

[display]
title = "Ligado há"
lines = 2                    # linhas da saída na tela (1 a 6)

[[mood]]                     # humor pela saída: a primeira regra que casar vale
contains = "week"
mood = "busy"
```

Saída vazia é `sleep`; o comando falhar é `alarm`; sem regra que case, `idle`.

Exemplo completo: [`examples/plugin-uptime`](examples/plugin-uptime).

## 2. Plugin com tela (`[ui]`)

Uma tela própria (`view.slint`, interpretada pelo hub) e a lógica num script
[Rhai](https://rhai.rs) (`main.rhai`), que roda isolado numa thread só dele.

```toml
uses = ["procs"]
[permissions]
exec = ["git"]               # o que run() pode executar
net = ["api.exemplo.com"]    # o que http_get() pode consultar (só https)
[ui]
view = "view.slint"
script = "main.rhai"
every = 10                   # fn tick() a cada N s (opcional; 1 a 3600)
```

O script responde a eventos e usa a API do hub:

| Do hub para o script | Do script para o hub |
|---|---|
| `fn start()` ao ligar | `ui("prop", valor)` muda a tela |
| `fn tick()` a cada `every` | `mood("happy")`, `summary("texto")` |
| `fn action(name, arg)` clique na tela | `run([...])`, `run_async([...], "fn")` |
| `fn on_event(topic, data)` provedores | `http_get(url)`, `http_get_async(url, "fn")` |
| `fn settings()` opção mudou | `store_get("k")`, `store_set("k", v)` (chave-valor) |
| `fn migrate(de, para)` dados de versão antiga | `db_batch(sql)`, `db_exec(sql, [params])`, `db_query(sql, [params])` (banco) |
| | `fact("strain", "CPU", 93)`, `notify("t", "texto")` |

O estado entre chamadas fica em `this` (`this.settings` traz as opções); o que tem de durar
vai para os dados do app ([abaixo](#dados-que-duram-data)). Limites: 5 milhões de operações
por chamada, `run` com prazo de 30 s, respostas HTTP de até 1 MiB, até 4 tarefas
assíncronas e 1 MiB no chave-valor. A referência completa está no topo de
[`src/apps/plugin/mod.rs`](src/apps/plugin/mod.rs) e, para o banco, de
[`src/apps/plugin/db.rs`](src/apps/plugin/db.rs).

Exemplos: [`examples/plugin-top`](examples/plugin-top) (processos pelo provedor `procs`,
sem executar nada), [`examples/plugin-notas`](examples/plugin-notas) (notas num banco
SQLite, com migração de versão) e os plugins da loja,
[Top processos](https://github.com/pedroluizmossi/miniutils-plugin-top-processes) e
[Pulso](https://github.com/pedroluizmossi/miniutils-plugin-pulso) (provedor `sys`, `ps` e
uma consulta HTTPS).

## 3. App externo (`[app]`, acesso total)

A lógica é um executável seu, em qualquer linguagem. O hub o inicia ao ligar o app, o
encerra ao desligar e conversa com ele por JSON, uma mensagem por linha, na entrada e na
saída padrão. A tela é um `view.slint`, como no plugin com tela.

```toml
uses = ["sys"]
[app]
view = "view.slint"
linux = "bin/meu-app"        # o executável de cada sistema, em bin/ (um basta)
windows = "bin/meu-app.exe"
every = 30                   # mensagem "tick" a cada N s (opcional; 1 a 3600)
```

Sem `[permissions]`: o app pode tudo o que o usuário pode.

| Do hub para o app (entrada) | Do app para o hub (saída) |
|---|---|
| `start` (id, sistema, pasta de dados, versão dos dados, opções) | `set` (propriedade da tela), `image` (PNG do disco) |
| `tick`, `action`, `event`, `settings` | `mood`, `summary`, `notify` |
| `shown` (a janela abriu) | `fact` (conversa entre mascotes) |
| `stop` (ou entrada fechada): saia | `error` (célula em alarme; `""` limpa) |
| | `migrated` (terminou de migrar os dados; ver abaixo) |

A saída de erro do app vai para o registro do hub (depuração é ali; a saída padrão é só do
protocolo). Se o app sair sozinho, o cartão fica em alarme e o botão de atualizar o inicia
de novo. Um app que trava ou cai não derruba o hub. O protocolo completo está no topo de
[`src/apps/external.rs`](src/apps/external.rs).

Exemplos:
- [`examples/app-hello`](examples/app-hello): o mínimo, em Rust, sem biblioteca: lê a
  pasta pessoal, guarda um contador na pasta de dados e reage à CPU.
- [miniutils-apps](https://github.com/pedroluizmossi/miniutils-apps): o app
  **Repositórios** (os seus repositórios Git e o que falta commitar e enviar) e o crate
  `miniutils-app`, que implementa o protocolo para Rust (`listen`, `set`, `mood`, `fact`…),
  além do workflow que compila e publica os executáveis por tag.

## 4. Mini-app embutido

Rust dentro do hub: implementa o trait `MiniApp`, tem a tela compilada junto
(`miniapps/<app>/view.slint`) e pode trazer um provedor novo para o barramento. Entra por
pull request neste repositório e sai em todas as instalações.

O passo a passo está em [DESIGN.md › Mini-apps](DESIGN.md#5-mini-apps) e o contrato em
[ARCHITECTURE.md › O mini-app](ARCHITECTURE.md#o-mini-app). Exemplos, do mais simples ao
mais completo:
- [`src/apps/todo.rs`](src/apps/todo.rs): sem provedor, com um arquivo na pasta de dados
  do app (`[data] kinds = ["files"]`);
- [`src/apps/ports.rs`](src/apps/ports.rs): provedor, ações em segundo plano e programas
  da allowlist;
- [`src/apps/vitals.rs`](src/apps/vitals.rs): várias mascotes, avisos sustentados e
  conversa entre mascotes.

## A tela (`view.slint`) de plugins e apps externos

- Exporta `component Main`. Pode importar `std-widgets.slint`, `miniutils.slint` (o tipo
  `Theme`) e as telas de `ui/` do próprio plugin; imagens só de `assets/`.
- O hub preenche, se a tela declarar: `in property <Theme> theme` (cores do tema do app,
  fontes do visual: `theme.font-ui`, `font-mono`, `font-title`, e o tamanho do texto
  escolhido pela pessoa: `font-size: 13px * theme.text-scale`), `in property <image>
  mascot` e `in property <string> mood`.
- Cliques chegam à lógica por `callback action(string, string)`.
- A célula vai de ~300 × 160 px (1×1 numa grade de 3 colunas) à janela inteira: esconda o
  secundário quando não couber e use `overflow: elide` em texto. Ver
  [DESIGN.md › Tamanhos](DESIGN.md#tamanhos).

## Dados que duram (`[data]`)

Cada app tem uma pasta de dados que o hub cuida, fora da pasta do app: uma atualização
troca a pasta do app inteira, e os dados ficam.

| Sistema | Pasta dos dados (uma subpasta por `id`) |
|---|---|
| Linux | `~/.local/share/miniutils/app-data/` (segue `XDG_DATA_HOME`) |
| Windows | `%APPDATA%\miniutils\app-data\` |

`miniutils --pastas` mostra o caminho (`MINIUTILS_DADOS` troca a pasta). O app diz o que
guarda no manifesto:

```toml
[data]
kinds = ["sqlite", "kv"]   # o que guarda (abaixo)
version = 1                # formato dos dados; subir pede migração
max_mb = 10                # cota do banco (1 a 1024; padrão 10)
```

| `kinds` | Quem usa | O que é |
|---|---|---|
| `kv` | plugin com tela | `store_get`/`store_set`: valores soltos (até 1 MiB), num `store.json` que o hub grava depois de cada chamada |
| `sqlite` | plugin com tela | `db_batch`/`db_exec`/`db_query`: um banco SQLite só do app (`data.sqlite`), com a cota de `max_mb` |
| `files` | app externo, mini-app embutido | a pasta, para o app gravar o que quiser (`data_dir` no `start`, `MINIUTILS_APP_DATA`) |

Sem `[data]`, o plugin com tela tem `kv` e o app externo tem `files`; o plugin simples não
guarda nada.

**O banco** (`sqlite`): estado, histórico, contas sobre muitos registros. Parâmetros com
`?` (`db_exec("INSERT INTO t VALUES (?, ?)", [nome, now()])`); lista ou mapa vira texto
JSON. O script fica dentro da pasta dele: `ATTACH`, `VACUUM INTO` e `PRAGMA` que muda o banco
são recusados (o `user_version` e a leitura da estrutura podem); cada instrução tem até 5 s
e cada consulta devolve até 10 mil linhas (use `LIMIT`); uma transação (`BEGIN`) aberta no
fim da chamada é desfeita. Na loja, o banco aparece nas permissões ("guarda: banco SQLite")
e o índice tem de anunciá-lo (`"sqlite": true`).

**Versão e migração.** Mudou o formato dos dados (tabela nova, coluna, outro JSON)? Suba o
`version`. Quando o app começa com dados de uma versão antiga, o hub copia a pasta para
`.copias/<id>/` e:
- plugin com tela: chama `fn migrate(de, para)` antes de `start()`;
- app externo: manda `data_version` (a versão dos dados) no `start`; o app migra e responde
  `{"type":"migrated","version":N}` com a versão nova.

Se a migração falhar (erro no `migrate`, o app sair antes do `migrated`), a cópia volta e o
cartão mostra o erro. Dados de uma versão **mais nova** que a do app (alguém voltou a uma
versão antiga) não são abertos: o app não começa. A instalação nova já começa na versão do
manifesto, sem migração: crie as tabelas no formato atual (`CREATE TABLE IF NOT EXISTS`) e
use `migrate` só para levar o formato antigo ao novo. Exemplo completo em
[`examples/plugin-notas`](examples/plugin-notas).

**Para quem usa:** em **Configurações**, nas opções de cada app, a linha **Dados** mostra o
que ele guarda e quanto, com **Exportar** (cópia em `Downloads/miniutils-dados/`) e
**Apagar dados** (o app para, os dados somem e ele recomeça do zero). Remover um app que
tem dados pergunta se eles ficam: os que ficam voltam numa reinstalação e aparecem em
**Configurações › Sistema › Dados de apps removidos** (`miniutils --remover <id>` pergunta
também, ou siga `--manter-dados` / `--apagar-dados`).

## Apps locais (sem a loja)

Um app não precisa vir da loja: o MiniUtils lê toda pasta de app que estiver na pasta dos
apps, a mesma em que a loja instala.

| Sistema | Pasta dos apps |
|---|---|
| Linux | `~/.local/share/miniutils/plugins/` (segue `XDG_DATA_HOME`) |
| Windows | `%APPDATA%\miniutils\plugins\` |

`miniutils --pastas` mostra o caminho certo no sistema em uso (`MINIUTILS_PLUGINS` troca a
pasta). As regras:

- **um app por pasta, e a pasta tem o nome do `id`** do manifesto:
  `plugins/io.github.fulano.meu-app/manifest.toml`. Com outro nome, o app é ignorado;
- depois de pôr, mudar ou apagar um app, rode `miniutils --recarregar` ou clique em
  **Configurações › Sistema › Recarregar** (o MiniUtils aberto relê a pasta);
- o app entra **desligado**: ligue-o na lista de apps das Configurações, onde ele aparece
  com o selo `local`;
- pastas recusadas aparecem em **Configurações › Sistema** e em `miniutils --plugins`, com
  o motivo (sem `manifest.toml`, pasta com nome diferente do `id`, erro no manifesto).

Um app local não passa pela revisão da loja: num app externo, o executável em `bin/` roda
com as suas permissões sem conferência de hash. Ponha só o que você escreveu ou conhece.

## Desenvolver

```sh
cargo run -- --plugin-dev <pasta do plugin ou app>
```

O app entra já ligado, na primeira página, e recarrega quando um arquivo muda (num app
externo, também quando o executável é recompilado). Os erros da tela e do script, e o que
um app externo escreve na saída de erro, saem no terminal. No modo dev, `bin/` de um app
externo pode ser um link para o `target/` do projeto dele. Feche o MiniUtils aberto antes:
o modo dev roda numa instância só dele.

## Publicar

- **Plugin** (simples ou com tela): repositório público com `manifest.toml`, `README.md` e
  `LICENSE`, e um pull request na
  [loja](https://github.com/pedroluizmossi/miniutils-store) acrescentando a entrada no
  `registry.json`, com o commit revisado e as permissões iguais às do manifesto (com banco,
  `"sqlite": true`).
- **App externo**: o código num repositório público e os executáveis num Release compilado
  pelo CI a partir de uma tag. A entrada da loja traz `"full_access": true` e, por sistema,
  o endereço e o SHA-256 do executável; o hub só instala se o hash bater. O workflow do
  [miniutils-apps](https://github.com/pedroluizmossi/miniutils-apps) gera essa entrada
  pronta. Precisa do MiniUtils 0.2.0 ou mais novo.
- **Mini-app embutido**: pull request neste repositório.

Como a loja revisa cada tipo: README da
[loja](https://github.com/pedroluizmossi/miniutils-store#o-que-%C3%A9-revisado).
