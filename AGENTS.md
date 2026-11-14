# AGENTS.md — como trabalhar neste projeto

Regras para agentes de código (Claude Code, Codex etc.). Arquitetura em
[ARCHITECTURE.md](ARCHITECTURE.md); visuais, temas, telas, apps e mascotes em
[DESIGN.md](DESIGN.md). Responda ao usuário em português do Brasil.

## Idioma: código em inglês

Tudo o que é código fica em **inglês** (convenção internacional; o projeto é aberto):

- identificadores: funções, tipos, campos, variáveis, módulos, propriedades e callbacks do
  Slint, funções do Rhai, nomes de teste;
- comentários e doc-comments (`//`, `///`, `//!`, `#`);
- mensagens de registro (`log::*!`), erros internos (`anyhow`/`context`/`bail!`) e
  `panic!`/`expect`;
- nomes de arquivos, pastas, flags de linha de comando e chaves de config/manifesto;
- mensagens de commit, nomes de branch e títulos/descrições de PR.

Exceções:

- **Nomes já gravados no disco ou no protocolo** (chaves de config, ids de mascote, valores
  de opção, pastas) não mudam sem motivo forte.

## Compatibilidade com versões antigas

O projeto é pré-1.0 e não guarda código de compatibilidade (ele foi removido na v0.4.0):

- **Sem alias nem migração de nome.** Pasta, arquivo, flag de linha de comando ou variável
  de ambiente renomeada passa a valer só com o nome novo; não acrescente tabela de nomes
  antigos, `rename` do lugar antigo nem leitura de reserva.
- **Formato da config é a exceção:** mudou a estrutura, suba `CURRENT_VERSION` e escreva o
  passo em `config::migrate` (`src/core/config.rs`). Versão abaixo de `OLDEST_VERSION` vira
  `.broken` e o app volta ao padrão; ao apagar um passo antigo, suba `OLDEST_VERSION`.
- **Dados dos apps** migram pelo mecanismo do produto (`[data] version` + `fn migrate`,
  `core/appdata.rs`), que não é legado.
- Leitores de formato de terceiros (ex.: logs antigos do Codex em
  `providers/agents/codex.rs`) ficam: o arquivo é do usuário, não nosso.
- **Falas das mascotes** (`lines.toml`) têm `pt`, `en` e `de` de propósito.

## Texto da interface (i18n: inglês + pt-BR)

Texto que o usuário vê (tela, bandeja, notificação, resposta de linha de comando) é escrito
em **inglês** e traduzido; registro (`log::*!`) não é traduzido. Detalhes em
`src/core/i18n.rs`.

- **Slint:** `text: @tr("Tasks")`; com valores, `@tr("{} of {}", a, b)`. Sem contexto
  (`"ctx" =>`) nem plural (`| … % n`): reescreva a frase.
- **Rust:** `tr!("Tasks")` (`&'static str`) ou `tr!("{} of {}", a, b)` (`String`). O 1º
  argumento é um literal `"…"` numa linha; marcadores só `{}`/`{0}`, nunca `{nome}`.
  Tabela de rótulos: `tr_noop!("Cyan")` na tabela e `i18n::tr(rótulo)` onde aparece.
- **Manifesto dos mini-apps embutidos:** `name`, `description` e `label` em inglês; o
  catálogo os traduz ao ler.
- **Tradução:** `i18n/pt-BR/<área>.po` (gettext, um arquivo por área; o `ui/build.rs` junta
  todos). O mesmo texto não pode ter duas traduções. O teste
  `catalog_covers_every_string` reprova texto sem tradução e entrada que ninguém usa.
- **Plugins e apps externos:** `@tr("…")` na tela e `i18n/pt-BR.po` na pasta do app (o hub
  traduz a tela e o manifesto ao carregar); app externo recebe `lang` no `start` e usa
  `miniutils_app::i18n::Catalog` para o resto.
- A troca de idioma em Configurações muda a interface compilada na hora; manifestos e telas
  de plugin, ao reiniciar.

## Compilar e validar

- **Valide com `cargo check`, não com `cargo build`.** O `check` também roda o
  compilador do Slint (erros de `.slint` aparecem nele) e leva segundos; o `build` só é
  preciso para rodar o app ou os testes.
- **Testes:** `cargo test`. Rode antes de dizer que terminou.
- **Clippy:** `cargo clippy --workspace --all-targets -- -D warnings` tem de passar (o CI
  reprova aviso). Um `#[allow(clippy::...)]` só com comentário dizendo por quê.
- **Uma compilação por vez.** Se o usuário estiver com `cargo run` aberto, um segundo
  cargo espera o lock ("Blocking waiting for file lock") — não é travamento.
- **Mudar o perfil no `Cargo.toml` recompila todas as dependências** (minutos). Não mexa
  em `[profile.*]` sem motivo. (Um `[profile.dev.package.<crate>]` recompila só aquele crate.)
- **Onde vai o tempo:** mudar só Rust recompila em ~2 s; mudar um `.slint` em ~11 s, quase
  tudo o rustc no `hub.rs` gerado (~10 MB), que por isso compila sem otimização
  (`[profile.dev.package.miniutils-ui]`; não pesou ao rodar). Para ajustar visual em série,
  use o live-preview (abaixo), que nem recompila.
- **Medir tempo de build:** use uma mudança real de conteúdo. `touch` num `.slint` gera o
  mesmo código e a compilação incremental reaproveita tudo; o número sai falso.

## Estrutura de crates

- `ui/` é o crate `miniutils-ui`: compila `ui/hub.slint` (e tudo o que ele importa,
  inclusive `miniapps/*/view.slint`) e reexporta os tipos gerados. O app usa
  `crate::ui::*`, que é só `pub use miniutils_ui::*`.
- O crate raiz (`miniutils`) tem o Rust do app. Mudança só de Rust não recompila a
  interface; mudança de `.slint` recompila `miniutils-ui` e depois o app.
- `ui/build.rs` liga `SLINT_ENABLE_EXPERIMENTAL_FEATURES` (o `ComponentContainer` que encaixa
  a tela dos plugins). O crate raiz usa `slint-interpreter` e `rhai` para os plugins
  (`src/apps/plugin/`); o interpretador tem de ser **exatamente** a mesma versão do `slint`.
- Tipos gerados pelo Slint vivem em outro crate: não dá para escrever `impl Trait for
  HubWindow` (ou outro tipo gerado) com um trait de fora; use função livre ou newtype.

## Plataformas (Linux e Windows)

O app é nativo nos dois. Toda mudança tem de compilar e passar nos testes nos dois (o CI em
`.github/workflows/ci.yml` roda `cargo check` e `cargo test` em Ubuntu e Windows).

- **Chamada de sistema não fica solta no código.** Tudo que depende do sistema mora atrás
  de uma interface comum, com uma implementação por sistema escolhida por `#[cfg]`:
  - módulo com `linux.rs` e `windows.rs`: `src/instance/`, `src/tray/`,
    `src/providers/sys/`, `src/providers/media/` (o arquivo de fora, ex. `src/tray.rs`, só
    escolhe a implementação e guarda o que é comum);
  - blocos `#[cfg(...)]` no próprio arquivo quando é pouco: `src/popup.rs`,
    `src/core/exec.rs`, `src/providers/ports.rs`;
  - o resto (encerrar processo, arquivo só do dono, pasta XDG): `src/core/os.rs`.
- **Recurso novo nasce nos dois.** Se um sistema não tem como (ex.: carga média no
  Windows), o campo fica vazio/zero e o comentário do campo diz isso; nada de `todo!()`.
- **Dependência só de um sistema** vai em `[target.'cfg(...)'.dependencies]` no
  `Cargo.toml` (`ksni`, `zbus`, `x11rb`, `libc` no Linux; `tray-icon`, `sysinfo`,
  `windows` no Windows).
- **Rodar programa:** sempre por `exec::command`/`exec::run_timeout`, nunca
  `Command::new` direto. No Windows é o que acha os `.cmd` do npm (`codex`, `claude`) e
  evita a janela de console piscando.
- **Caminhos:** `Path::join` e `dirs::*`, nunca `$HOME` nem `/` montado em string. Em
  teste, compare caminhos trocando `\` por `/`.
- **Testes:** o que usa `sh`, `echo` ou link simbólico leva `#[cfg(unix)]`. Leitores de
  saída de comando (`parse_netstat`, `windows_file_names`) são funções puras compiladas
  também nos testes do Linux (`#[cfg(any(windows, test))]`): teste-os aqui.
- **Conferir o Windows a partir do Linux:** `rustup target add x86_64-pc-windows-gnu`,
  o pacote `mingw-w64-gcc` e `cargo check --target x86_64-pc-windows-gnu` pegam erro de
  compilação. Comportamento (bandeja, popup, mídia) só num Windows de verdade: peça ao
  usuário para conferir.

## Interface (Slint)

- **Ver e mexer na interface:** `tools/dev-ui.sh` (só Linux; ajuda no topo do arquivo)
  abre um MiniUtils de desenvolvimento isolado (outra instância, config e registro em
  `target/dev-ui`) com o servidor MCP do Slint (recurso `dev-mcp`), e daí tira captura
  (`shot`, leia o PNG), acha elementos (`find`), clica (`click`) e chama qualquer
  ferramenta do MCP (`mcp`). Mudança visual: confira com `shot` antes de dizer que ficou
  certo, e diga em que tamanho de janela/célula conferiu. Plugin: `open -- --plugin-dev
  <pasta>`. Ajuste em série: `open --live` (recurso `live-preview`), edite o `.slint`,
  `reload`, `shot`; os limites do live-preview estão no topo do script.
- **Bug que o usuário relata** (visual ou de clique): pergunte o sintoma exato (o que
  abre, o que acontece ao clicar) e reproduza com o `dev-ui.sh` antes de mexer. Não capture
  a tela do usuário (do desktop) sem ele pedir: pode pegar outra janela; o `dev-ui.sh` só
  captura a janela do app de desenvolvimento.
- **Começar um app novo:** a skill `new-app` (`.claude/skills/new-app/`) diz qual exemplo
  copiar para cada tipo e como conferir na tela.
- **Testes de UI sem tela** (`i-slint-backend-testing`) precisam de
  `SLINT_EMIT_DEBUG_INFO=1` na compilação, e cliques simulados em `PopupWindow` não
  funcionaram (nem num caso normal). Não confie neles para listas suspensas.
- **Tokens, não valores soltos:** cores e medidas vêm de `Skin` (visual da janela) e
  `Colors` (tema do app), em `ui/theme.slint`.
- **Número em texto que não é para ler (caminho de `Path`):** o Slint escreve `float` no
  idioma da interface (`0,5` em pt-BR), e o caminho fica inválido. Monte o `commands` com
  inteiros (`round`, ex.: `ui/frame.slint`) ou gere-o no Rust com `format!`.
- **Texto escala:** `font-size: 13px * Skin.text-scale;` (tamanho do texto da
  Aparência). Altura mínima de linha é `max(44px, self.preferred-height)`, nunca `44px`
  fixo num layout (corta o texto que quebra). Ver `DESIGN.md › Tamanho do texto`.
- **Mascotes:** o componente `Mascot` (`ui/widgets.slint`) usa `Skin.mascot-size` como
  tamanho padrão e escala o quadro 16×14 para caber na caixa que recebe, sem deformar.
  Um app só dá `width`/`height` quando quer outra caixa, e deriva do token
  (ex.: `Skin.mascot-size * 2 / 3` na versão compacta). Ao mudar algo compartilhado,
  revise **todos** os usos (`grep -rn "Mascot {" ui miniapps`) e diga quais mudou.
- **Opções de app** vêm do `manifest.toml` de cada mini-app (`bool`, `int`, `choice`).
  Para escolha entre duas opções, prefira `bool`.

## Registro

- **Mensagem para o registro: `log::warn!`/`log::info!`/`log::debug!`, nunca `eprintln!`.**
  Vai para o arquivo (`core/logging.rs`) e para o terminal; no Windows não há console, então
  um `eprintln!` some. `println!`/`eprintln!` só nos comandos de terminal (`--help`, loja,
  `--dirs`), em que a saída é a resposta.
- Plugin que precisa de processos usa `uses = ["procs"]`, não `ps` (que não existe no
  Windows).

## Hábitos

- Leia o código antes de propor; confira o que um resumo afirma (ex.: "todos os apps
  usam X") com `grep` antes de dizer.
- Hipótese que um teste ou leitura derrubou: diga que estava errada, não deixe de pé.
- Não deixe testes ou dependências de diagnóstico no projeto se não servirem.
