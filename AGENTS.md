# AGENTS.md — como trabalhar neste projeto

Regras para agentes de código (Claude Code, Codex etc.). Arquitetura em
[ARCHITECTURE.md](ARCHITECTURE.md); visuais, temas, telas, apps e mascotes em
[DESIGN.md](DESIGN.md). Responda e comente o código em português do Brasil.

## Compilar e validar

- **Valide com `cargo check`, não com `cargo build`.** O `check` também roda o
  compilador do Slint (erros de `.slint` aparecem nele) e leva segundos; o `build` só é
  preciso para rodar o app ou os testes.
- **Testes:** `cargo test`. Rode antes de dizer que terminou.
- **Uma compilação por vez.** Se o usuário estiver com `cargo run` aberto, um segundo
  cargo espera o lock ("Blocking waiting for file lock") — não é travamento.
- **Mudar o perfil no `Cargo.toml` recompila todas as dependências** (minutos). Não mexa
  em `[profile.*]` sem motivo.
- **Medir tempo de build:** use uma mudança real de conteúdo. `touch` num `.slint` gera o
  mesmo código e a compilação incremental reaproveita tudo; o número sai falso.

## Estrutura de crates

- `ui/` é o crate `miniutils-ui`: compila `ui/hub.slint` (e tudo o que ele importa,
  inclusive `miniapps/*/view.slint`) e reexporta os tipos gerados. O app usa
  `crate::ui::*`, que é só `pub use miniutils_ui::*`.
- O crate raiz (`miniutils`) tem o Rust do app. Mudança só de Rust não recompila a
  interface; mudança de `.slint` recompila `miniutils-ui` e depois o app.
- Tipos gerados pelo Slint vivem em outro crate: não dá para escrever `impl Trait for
  HubWindow` (ou outro tipo gerado) com um trait de fora; use função livre ou newtype.

## Interface (Slint)

- **Você não vê a janela.** Para qualquer mudança visual: diga ao usuário que não viu o
  resultado e peça para ele conferir. Se ele relata um bug visual ou de clique, **pergunte
  primeiro o sintoma exato** (o que abre, o que acontece ao clicar) antes de investigar.
  Não capture a tela do usuário sem ele pedir: pode pegar outra janela.
- **Testes de UI sem tela** (`i-slint-backend-testing`) precisam de
  `SLINT_EMIT_DEBUG_INFO=1` na compilação, e cliques simulados em `PopupWindow` não
  funcionaram (nem num caso normal). Não confie neles para listas suspensas.
- **Tokens, não valores soltos:** cores e medidas vêm de `Skin` (visual da janela) e
  `Colors` (tema do app), em `ui/theme.slint`.
- **Mascotes:** o componente `Mascot` (`ui/widgets.slint`) usa `Skin.mascot-size` como
  tamanho padrão e escala o quadro 16×14 para caber na caixa que recebe, sem deformar.
  Um app só dá `width`/`height` quando quer outra caixa, e deriva do token
  (ex.: `Skin.mascot-size * 2 / 3` na versão compacta). Ao mudar algo compartilhado,
  revise **todos** os usos (`grep -rn "Mascot {" ui miniapps`) e diga quais mudou.
- **Opções de app** vêm do `manifest.toml` de cada mini-app (`bool`, `int`, `choice`).
  Para escolha entre duas opções, prefira `bool`.

## Hábitos

- Leia o código antes de propor; confira o que um resumo afirma (ex.: "todos os apps
  usam X") com `grep` antes de dizer.
- Hipótese que um teste ou leitura derrubou: diga que estava errada, não deixe de pé.
- Não deixe testes ou dependências de diagnóstico no projeto se não servirem.
