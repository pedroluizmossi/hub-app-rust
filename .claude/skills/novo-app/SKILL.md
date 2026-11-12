---
name: novo-app
description: Cria um app novo do MiniUtils (plugin simples, plugin com tela, app externo ou mini-app embutido) a partir dos exemplos do projeto, e só entrega depois de vê-lo funcionando na tela. Use quando o usuário pedir um app, plugin, widget, cartão ou mini-app novo para o hub, mesmo sem dizer o tipo.
---

# Novo app do MiniUtils

O guia de referência é o `APPS.md` (tipos, manifesto, provedores, API do script e do
protocolo). Esta skill diz **por onde começar, o que copiar e como conferir**; não repita
o que está lá, leia.

## 1. Escolher o tipo

Use "Qual escolher" do `APPS.md`: o mais simples que resolve.

| Pedido | Tipo | Copie de |
|---|---|---|
| mostrar a saída de um comando | plugin simples | `examples/plugin-uptime` |
| tela própria, botões, dados de provedores, HTTPS | plugin com tela | `examples/plugin-top` |
| arquivos, sockets, rede local, processo longo, outra linguagem | app externo | `examples/app-hello` |
| parte do produto, provedor novo, tela compilada | mini-app embutido | `miniapps/todo` + `src/apps/todo.rs` |

Se o pedido couber em dois tipos, ou se faltar onde criar a pasta, faça **uma** pergunta só
(tipo e pasta juntos). Plugins e apps externos são repositórios próprios: sugira
`../miniutils-plugin-<nome>` (ou `../miniutils-app-<nome>`), fora deste repositório.
Embutido fica aqui.

**Só para uso próprio, sem publicar** (ex.: quem instalou o MiniUtils pelo `install.sh` e
não tem este repositório): crie o plugin direto na pasta dos apps, que o MiniUtils lê
sozinho. O caminho sai de `miniutils --pastas` (linha "apps locais"); a pasta do app tem
de se chamar exatamente o `id`. Ver `APPS.md › Apps locais`.

## 2. Criar

**Plugin ou app externo**
1. Copie a pasta do exemplo inteira (manifesto, `README.md`, `LICENSE`, tela, script ou
   `src/`). O app externo, fora deste repositório, é um crate sozinho: o link de `bin/`
   aponta para o `target/` dele (`ln -sf ../target/debug/<nome> bin/<nome>`), não para o
   daqui como no README do `app-hello`.
2. `id = "io.github.<dono>.<nome>"` (dono: o do `git remote` deste repositório, se o
   usuário não disser outro). `cyber.*` é só dos embutidos.
3. `icon`: uma mascote existente (`key:` em `src/mascot/sprites.rs`). Leia a lista; não
   invente nome.
4. `uses` e `[permissions]` só com o que o app usa de fato (a loja mostra isso ao instalar).
5. Tela: `component Main`, cores de `theme`, `font-size: 13px * theme.text-scale`,
   `overflow: elide` em texto, e esconda o secundário em célula pequena (~300 × 160 px).
   Ver "A tela" no `APPS.md`.

**Mini-app embutido**: siga `DESIGN.md › 5. Mini-apps`, com o To-do de modelo. Os pontos
que um app novo toca (confira com `grep -rn todo` se algo mudou de lugar):
- `miniapps/<app>/manifest.toml` (`id = "cyber.<nome>"`, opção `theme` com `default =
  "auto"` e sem `options`: o hub preenche os temas) e `miniapps/<app>/view.slint`
  (`export global <App>App` + `export component <App>View`; `font-size` sempre
  `× Skin.text-scale`);
- `src/apps/<app>.rs` (struct com `create(host, ui)` + trait `MiniApp`) e `pub mod` em
  `src/apps/mod.rs`;
- `src/hub/mod.rs`: `registry.add(crate::apps::<app>::<App>Mini::create(host.clone(), &ui) as AppRef, None);`
  junto dos outros;
- `ui/dashboard.slint`: importar a view e um `if c.id == "cyber.<nome>": <App>View { … }`;
- `ui/hub.slint`: importar o global e as structs da tela e pô-los no `export { … }`;
- `src/core/manifest.rs`: o manifesto novo na lista do teste `manifestos_embutidos_sao_validos`.

## 3. Conferir (não entregue sem isto)

O `tools/dev-ui.sh` abre um MiniUtils isolado (não mexe na instância nem na config do
usuário) e deixa você **ver** a tela. Ver `AGENTS.md › Ver e mexer na interface`.

**Plugin ou app externo**
```sh
tools/dev-ui.sh abrir -- --plugin-dev <pasta>   # entra ligado, na 1ª página
tools/dev-ui.sh foto                            # e leia o PNG com Read
tools/dev-ui.sh log                             # erros da tela, do script, stderr do app
```
O modo dev recarrega ao salvar: edite, espere ~1 s e tire outra `foto`. Clique nos botões
da tela (`tools/dev-ui.sh achar <texto>`, `clicar <texto>`) e confira a reação.

Sem este repositório (sem `tools/dev-ui.sh`), com o app na pasta dos apps: rode
`miniutils --recarregar` e confira em `miniutils --plugins` que ele não está entre os
ignorados; peça ao usuário para ligá-lo em Configurações e olhar a tela.

**Mini-app embutido**
1. `cargo check`, depois `cargo test` e `cargo clippy --workspace --all-targets -- -D warnings`.
2. `tools/dev-ui.sh abrir`, ponha o app numa página (Configurações › apps, por
   `clicar`; ou edite `target/dev-ui/config/miniutils/config.toml` e abra de novo) e
   `foto`.
3. Ajuste visual em série: `tools/dev-ui.sh abrir --live`, edite o `view.slint`,
   `tools/dev-ui.sh recarregar`, `foto` (cada volta leva segundos, sem recompilar).

## 4. Entregar

Diga o tipo escolhido e por quê, os arquivos criados, o que foi conferido (com a captura)
e o que não foi (ex.: Windows, que só se confere num Windows de verdade). Para publicar,
aponte "Publicar" no `APPS.md`; não abra pull request na loja sem o usuário pedir.
