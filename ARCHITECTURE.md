# MiniUtils — arquitetura

MiniUtils é um **hub de mini-apps** para o desktop Linux: um único binário Rust que fica na
bandeja do sistema e hospeda pequenos apps (Docker, sinais vitais, música, …) que você liga,
desliga e arruma numa grade. Cada app tem uma mascote em pixel art que reage ao estado dele.
Nasceu como reescrita do plasmoid CyberDock, mas não depende do KDE: roda em KDE, XFCE,
Cinnamon, MATE e GNOME (este com a extensão AppIndicator para a bandeja).

O visual não é fixo: a janela troca de "visual" (Neon, Terminal, Aconchego, Noite chuvosa…)
e cada app tem o próprio tema de cores. Como criar visuais, temas, telas, apps e mascotes
está em [DESIGN.md](DESIGN.md).

## Princípios

1. **Um processo, um binário.** Nada de daemon + cliente, nada de um processo por mini-app.
   Simples de instalar, de depurar e leve (~25 MB na bandeja, ~30 MB com a janela aberta).
2. **Mini-app desligado não custa nada.** Não desenha, não tem timer, não coleta dado.
3. **Cada dado é coletado uma vez só.** Os provedores (CPU, GPU, Docker, mídia…) são
   compartilhados; só rodam enquanto algum mini-app ativo os assina.
4. **O host oferece os serviços; o mini-app só monta a tela e a lógica dele.** Executor de
   comandos, configuração, temas, mascotes e notificações são do host.
5. **Seguro por padrão.** Mini-app só executa os programas declarados no manifesto
   (`permissions.exec`). Argumentos são passados sem shell — nada de interpolar strings.
6. **Plataforma depois dos produtos.** O contrato de plugin nasce dos mini-apps reais, não o
   contrário. Por isso a fase 1 compila os mini-apps junto; plugins externos vêm depois.
7. **Aparência é dado, não código.** Visuais da janela e temas dos apps são tabelas de
   valores; a tela lê tokens (`Skin`, `Colors`) e nunca cores soltas.

## Visão geral

```
 Superfícies   [ícone na bandeja (ksni/SNI)]   [popup do hub]   [widget flutuante*]
                            │                        │
 ┌──────────────────────────┴────────────────────────┴───────────────────────────┐
 │ miniutils (binário Rust único)                                                 │
 │                                                                                │
 │  UI (Slint)    Janela: moldura e cabeçalho do visual (Skin) · painel em grade  │
 │                · modo de edição · configurações                                │
 │                Views dos mini-apps (.slint compilado; interpretado*)           │
 │                                                                                │
 │  Núcleo        Registro de mini-apps ── manifest.toml, liga/desliga            │
 │                Serviços: Executor (allowlist) · Config (TOML, versionada) ·    │
 │                          Visuais + temas + mascotes · Notificações · Grade     │
 │                Provedores: sys · gpu · docker · media (MPRIS)                  │
 │                           (thread própria, coleta 1×, distribui)               │
 └────────────────────────────────────────────────────────────────────────────────┘
   /proc + /sys + hwmon     nvidia-smi / amdgpu      docker CLI      D-Bus (MPRIS)
                                                                    * = fase 2
```

## Estrutura de pastas

```
hub-app-rust/
  Cargo.toml                  pacote `miniutils`
  build.rs                    compila ui/hub.slint (e tudo que ele importa)
  ARCHITECTURE.md             este documento
  DESIGN.md                   visuais, temas, telas, apps e mascotes: como criar
  ui/
    hub.slint                 janela: moldura, cabeçalhos, painel (grade + edição), configurações
    theme.slint               tokens: Skin (visual da janela), Colors (tema de app), Icons, Anim
    widgets.slint             componentes: Mascot, Icon, IconButton, TextButton, HeaderButton,
                              SqSwitch, Stepper, Select, SegBar, Sparkline, Counter, SearchBox
    fonts/                    Chakra Petch, JetBrains Mono, Pixelify Sans (+ licenças OFL)
  miniapps/
    <app>/manifest.toml       manifesto (id, provedores, permissões, configurações)
    <app>/view.slint          tela do app (global <App>App + componente <App>View)
    docker/ vitals/ cdplayer/
  src/
    main.rs                   argumentos (--hidden, --tab) e event loop
    hub.rs                    o host: liga config, registro, provedores, janela, bandeja,
                              grade, modo de edição, visual, relógio
    instance.rs               instância única por socket Unix (a 2ª só pede para abrir)
    ui.rs                     tipos gerados do Slint
    tray.rs                   ícone na bandeja com a mascote do app em pior estado
    popup.rs                  popup de bandeja: posição, fora da barra de tarefas, X11/XWayland
    core/
      miniapp.rs              o trait MiniApp e o Host (serviços)
      manifest.rs             formato do manifest.toml
      registry.rs             liga/desliga mini-apps e distribui eventos
      config.rs               ~/.config/miniutils/config.toml (versão, migrações, nome exibido)
      layout.rs               grade do painel: posições, encaixe ao arrastar (settle), vagas
      exec.rs                 executor com allowlist
      notify.rs               notificações (no máximo 1/min por evento)
      bus.rs                  provedores em threads, assinaturas, "poke" para ler já
    providers/
      sys.rs                  CPU (uso, GHz, carga), RAM e swap, temperatura, rede
                              (interface, IP), partições e leitura/escrita dos discos
      gpu.rs                  nvidia-smi ou amdgpu via sysfs
      docker.rs               docker ps / stats / events
      media.rs                MPRIS pelo D-Bus: o que está tocando e comandos do player
    skin/
      mod.rs                  visuais da janela (tabela SKINS) e `apply` para o global Skin
      backdrop.rs             fundos desenhados em Rust (pontos, CRT, papel, cidade)
    mascot/
      sprites.rs              sprites: 12 por papéis de cor (16×14, inclui o CD) + 20 "pixel" (12×12)
      themes.rs               temas de cor dos apps
      render.rs               sprite + tema + humor + quadro -> imagem RGBA 16×14
      disc.rs                 o CD 24×24 do app de música (desenho próprio, não é mascote)
    apps/
      docker.rs  vitals.rs    lógica de cada mini-app
      cdplayer.rs             CD Player: faixa, progresso, comandos e humor
```

## O mini-app

Um mini-app é **manifesto + tela + lógica**.

```toml
# miniapps/docker/manifest.toml
id = "cyber.docker"
name = "Docker"
description = "Containers e stacks com ações rápidas; a mascote reage quando algo cai."
icon = "neko"            # mascote do app (configurações, bandeja)
api = 1                  # versão do contrato com o host
uses = ["docker"]        # provedores que assina enquanto ativo

[permissions]
exec = ["docker"]        # programas que pode executar

[[settings]]
key = "theme"            # todo app tem um tema; "auto" acompanha o visual da janela
label = "Tema"
type = "choice"
default = "auto"
options = ["auto", "neon", "matrix", "aconchego"]
```

- `uses` liga os provedores quando o app é ativado e os solta quando é desativado.
- `permissions.exec` é a allowlist do executor para aquele app.
- `settings` vira formulário nas **Configurações** (tipos `bool`, `int`, `choice`). Um
  rótulo terminado em ` (%)` ou ` (°C)` vira a unidade do stepper.

O lado Rust implementa o trait `MiniApp` (`src/core/miniapp.rs`). Todas as chamadas são na
thread da UI; um app desligado não recebe nada.

```rust
pub trait MiniApp {
    fn manifest(&self) -> &Manifest;
    fn enable(&mut self, ui: &HubWindow) {}              // ligou: prepara estado, preenche a tela
    fn disable(&mut self, ui: &HubWindow) {}             // desligou: solta o que segura
    fn on_event(&mut self, ev: &Event, ui: &HubWindow);  // leitura de um provedor
    fn tick(&mut self, frame: u32, ui: &HubWindow) {}    // quadro de animação (~8/s, janela visível)
    fn settings_changed(&mut self, ui: &HubWindow) {}    // mudou configuração, tema ou visual
    fn mood(&self) -> Mood;                              // sleep < idle < happy < busy < alarm
    fn mascot_key(&self) -> String { /* icon */ }        // mascote na bandeja
    fn summary(&self) -> String;                         // linha do tooltip da bandeja
}
```

O `Host` oferece `config`, `bus`, `notifier`, `theme_for(manifest)` (resolve "auto" pelo
visual) e `exec_for(manifest)` (executor com a allowlist do app).

A bandeja mostra a mascote e o tema do app em **pior** humor.

Como criar um app novo, passo a passo: [DESIGN.md › Mini-apps](DESIGN.md#5-mini-apps).

## Fluxo de dados

```
provedor (thread) ──snapshot──► canal ──► thread da UI (timer de 100 ms esvazia o canal)
                                              │
                                  para cada app ativo que usa o provedor:
                                      app.on_event() → globals/modelos do Slint
```

- Provedores rodam em threads próprias (`bus.spawn` / `spawn_polled`) e dormem quando
  ninguém assina. `poke` pede uma leitura imediata (ex.: depois de parar um container).
- O Docker é por eventos (`docker events`), com `ps` de reserva a cada 15 s e `stats` a cada
  5 s só com containers rodando.
- A UI só é tocada na thread do event loop do Slint.
- O quadro de animação (mascotes, cursor do Terminal) roda só com a janela visível: 8/s com
  o mouse sobre a janela, 4/s fora. Flutuar e pular são animações do Slint, sem redesenhar.
- Listas do Slint só são recriadas quando o conteúdo muda de fato: recriar uma lista destrói
  os componentes dentro dela (um select aberto fecha). Ver `rebuild_settings`.

## Janela, grade e visual

- **Telas:** `current` = `"panel"` (painel) ou `"apps"` (configurações). O modo de edição é
  um estado do painel (`editing`). Esc volta das configurações, conclui a edição ou fecha o popup.
- **Grade:** colunas × linhas (até 6 × 6); cada app guarda `x`, `y`, `w`, `h` na config.
  `layout::settle` resolve um arrasto (o app arrastado fica; os outros ficam onde estão ou vão
  para a primeira vaga; se não couber, o arrasto é recusado). `Config::arrange` garante lugar
  para apps religados.
- **Visual:** `skin::apply` preenche o global `Skin` (cores, fontes, moldura, cabeçalho,
  botões); `skin::backdrop::render` desenha o fundo da área do painel numa imagem, refeita
  quando a área muda de tamanho. Os cantos cortados do visual Neon ficam transparentes.

## Configuração

`~/.config/miniutils/config.toml`, versionada (`version`). Ao mudar a estrutura, suba
`VERSAO_ATUAL` e acrescente a migração em `config::migra`; o arquivo antigo vira `.bak`.
Arquivo de versão mais nova é só lido (nunca sobrescrito); TOML quebrado vira `.quebrado`.

| Versão | Mudança |
|---|---|
| 0 → 1 | `theme` global vira tema de cada app |
| 1 → 2 | a grade passa a guardar posição (`x`, `y`) e ganha `rows` |

Campos sem migração (têm padrão): `name` (nome exibido, gravado ao abrir se faltar), `skin`.
Na primeira execução, a config de `~/.config/cyberhub/` (nome antigo) é copiada.

## Roteiro

**Fase 1 (atual)** — mini-apps compilados junto, contrato estável:
- [x] Manifesto, registro, ligar/desligar, configuração em TOML com migrações
- [x] Provedores sys, gpu, docker e media compartilhados
- [x] Mini-apps Docker e Sinais vitais (com avisos sustentados)
- [x] Mini-app CD Player (MPRIS: faixa, progresso, comandos, notificação opcional)
- [x] Bandeja com a mascote no humor do hub, notificações, instância única
- [x] Grade com arrastar/redimensionar, visuais da janela, temas claros, 31 mascotes

**Fase 2** — plugins externos sem recompilar:
- Views em `.slint` carregadas pelo `slint-interpreter` a partir de `~/.local/share/miniutils/miniapps/`
- Lógica em Rhai, com acesso só aos serviços do host (executor com allowlist, config, notify)
- Widget flutuante na área de trabalho (layer-shell no KDE/wlroots; janela comum no GNOME)

**Fase 3 (talvez)** — expor o hub por D-Bus para cascas nativas (plasmoid, extensão GNOME).

## Decisões e por quês

| Decisão | Por quê |
|---|---|
| Rust + Slint | Leve para ficar sempre ligado; `.slint` é declarativo como o QML do CyberDock |
| Renderizador por software do Slint | Com o femtovg (OpenGL) o processo carregava o driver da GPU: ~270 MB de RSS. Por software: ~30 MB. Custo: sem blur nem sombra, gradiente só com 2 cores, `Path` sem rotação (ver DESIGN.md › Limites) |
| Fundos desenhados em Rust | O renderizador por software não tem padrões repetidos nem gradientes radiais baratos; uma imagem pronta custa uma cópia por quadro |
| Mascote desenhada em Rust | Pixel art nítida em qualquer escala (`image-rendering: pixelated`); humor, piscar e glitch mudam pixels sem assets |
| Visual e tema como tabelas | Criar um visual ou tema novo não mexe em layout (princípio 7) |
| `docker` CLI em vez de `bollard` | Paridade com o CyberDock, menos dependências (sem hyper/tokio), passa pela allowlist |
| MPRIS pelo `zbus` | O `ksni` já traz o zbus; nenhuma dependência nova |
| Threads em vez de tokio | Poucos provedores com polling/eventos não justificam um runtime async |
| Mini-apps compilados na fase 1 | Contrato de plugin nasce de apps reais (princípio 6) |
| Popup de bandeja via X11/XWayland | O Wayland não deixa o app escolher onde a janela abre. Com `$DISPLAY` o hub roda como cliente X11: abre junto do ícone clicado, sem moldura, fora da barra de tarefas e some ao perder o foco ou com Esc. `MINIUTILS_NATIVE_WAYLAND=1` força Wayland nativo (sem posição) |
| Instância única por socket | Abrir de novo (atalho, autostart) só traz a janela da instância que já roda |
| `ksni` para a bandeja | StatusNotifierItem puro em Rust (D-Bus); funciona em KDE, XFCE, Cinnamon, MATE e GNOME com AppIndicator |
| Licença do Slint | Usado sob a *Royalty-free Desktop License* (exige crédito ao Slint no "Sobre"); alternativa: GPLv3 |
