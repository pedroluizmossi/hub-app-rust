# CyberHub — arquitetura

CyberHub é um **hub de mini-apps** para o desktop Linux: um único binário Rust que fica na
bandeja do sistema e hospeda pequenos apps (Docker, sinais vitais, …) que você liga e desliga
à vontade. É a evolução do plasmoid CyberDock, mas sem depender do KDE: roda em KDE, XFCE,
Cinnamon, MATE e GNOME (este com a extensão AppIndicator para a bandeja).

## Princípios

1. **Um processo, um binário.** Nada de daemon + cliente, nada de um processo por mini-app.
   Simples de instalar, de depurar e leve (medido: ~25 MB na bandeja, ~30 MB com a janela aberta).
2. **Mini-app desligado não custa nada.** Não desenha, não tem timer, não coleta dado.
3. **Cada dado é coletado uma vez só.** Os provedores (CPU, GPU, Docker…) são compartilhados;
   só rodam enquanto algum mini-app ativo os assina.
4. **O host oferece os serviços; o mini-app só monta a tela e a lógica dele.** Executor de
   comandos, configuração, temas/mascotes e notificações são do host.
5. **Seguro por padrão.** Mini-app só executa os programas declarados no manifesto
   (`permissions.exec`). Argumentos são passados sem shell — nada de interpolar strings.
6. **Plataforma depois dos produtos.** O contrato de plugin nasce dos mini-apps reais, não o
   contrário. Por isso a fase 1 compila os mini-apps junto; plugins externos vêm depois.

## Visão geral

```
 Superfícies     [ícone na bandeja (ksni/SNI)]  [popup do hub]  [widget flutuante*]
                              │                       │
 ┌────────────────────────────┴───────────────────────┴──────────────────────────┐
 │ cyberhub (binário Rust único)                                                 │
 │                                                                               │
 │  UI (Slint)          Shell do hub (abas + gerenciador de apps)                │
 │                      Views dos mini-apps (.slint compilado; interpretado*)    │
 │                                                                               │
 │  Núcleo              Registro de mini-apps ── lê manifest.toml, liga/desliga  │
 │                      Serviços: Executor (allowlist) · Config (TOML) ·         │
 │                                Temas + mascotes · Notificações                │
 │                      Provedores: sys (CPU/RAM/temp/rede) · gpu · docker       │
 │                                  (thread própria, coleta 1×, distribui)       │
 └───────────────────────────────────────────────────────────────────────────────┘
      /proc + hwmon        nvidia-smi / amdgpu (sysfs)        docker CLI
                                                                  * = fase 2
```

## Estrutura de pastas

```
hub-app-rust/
  Cargo.toml
  build.rs                    compila ui/hub.slint
  ARCHITECTURE.md             este documento
  ui/
    hub.slint                 janela do hub: cabeçalho, abas, aba "Apps"
    theme.slint               global Theme (cores do tema ativo)
    widgets.slint             Mascot, Bar, Sparkline, Pill… reutilizáveis
  miniapps/
    docker/manifest.toml      manifesto do mini-app Docker
    docker/view.slint         tela do mini-app Docker
    vitals/manifest.toml
    vitals/view.slint
  src/
    main.rs                   argumentos (--hidden, --tab) e event loop
    hub.rs                    o host: liga config, registro, provedores, janela e bandeja
    ui.rs                     tipos gerados do Slint
    core/
      miniapp.rs              o trait MiniApp e o Host (serviços)
      manifest.rs             formato do manifest.toml
      registry.rs             descobre e liga/desliga mini-apps
      config.rs               ~/.config/cyberhub/config.toml
      exec.rs                 executor com allowlist
      notify.rs               notificações (com limite de 1/min por evento)
      bus.rs                  assinaturas dos provedores (contagem de referências)
    providers/
      sys.rs                  /proc/stat, /proc/meminfo, hwmon, /proc/net/dev
      gpu.rs                  nvidia-smi ou amdgpu via sysfs
      docker.rs               docker ps / stats / system df
    mascot/
      sprites.rs              as 11 mascotes em pixel art (portadas do CyberDock)
      themes.rs               os 6 temas
      render.rs               sprite + humor + quadro -> imagem RGBA
    apps/
      docker.rs               lógica do mini-app Docker (lista, eventos, ações)
      vitals.rs               lógica do mini-app Sinais vitais
    tray.rs                   ícone na bandeja com a mascote no humor do hub
    popup.rs                  popup de bandeja: posição, fora da barra de tarefas, X11/XWayland
```

## O mini-app

Um mini-app é **manifesto + tela + lógica**:

```toml
# miniapps/docker/manifest.toml
id = "cyber.docker"
name = "Docker"
description = "Containers, stacks e ações rápidas"
icon = "neko"            # mascote usada na aba
api = 1                  # versão do contrato com o host
uses = ["docker", "sys"] # provedores que assina enquanto ativo

[permissions]
exec = ["docker"]        # programas que pode executar

[[settings]]
key = "hide_stopped"
label = "Esconder parados"
type = "bool"
default = false
```

- `uses` liga os provedores quando o app é ativado e os solta quando é desativado.
- `permissions.exec` é a allowlist do executor para aquele app.
- `settings` vira formulário automaticamente na aba **Apps** (tipos `bool`, `int`, `choice`).

O lado Rust implementa o trait `MiniApp`:

```rust
pub trait MiniApp {
    fn manifest(&self) -> &Manifest;
    fn on_enable(&mut self, ctx: &HostCtx);   // assina provedores, prepara estado
    fn on_disable(&mut self);                 // solta tudo
    fn on_data(&mut self, ev: &ProviderEvent, ui: &HubWindow); // atualiza a tela
    fn mood(&self) -> Mood;                   // humor para a mascote do hub/bandeja
}
```

O humor do hub (ícone na bandeja) é o **pior** humor entre os apps ativos:
`sleep < idle < busy < alarm`.

## Fluxo de dados

```
provedor (thread) ──snapshot──► canal ──► thread da UI (invoke_from_event_loop)
                                              │
                                  para cada app ativo que usa o provedor:
                                      app.on_data() → modelos Slint
```

- Provedores rodam em threads próprias com `std::thread` e dormem quando ninguém assina.
- A UI só é tocada na thread do event loop do Slint.
- A animação das mascotes é desenhada em Rust (16×14 px → `SharedPixelBuffer`) num timer
  de UI que só roda com a janela visível; flutuar/pular é animação do Slint (sem redesenho).

## Roteiro

**Fase 1 (atual)** — mini-apps compilados junto, contrato estável:
- [x] Manifesto, registro, ligar/desligar, configuração em TOML
- [x] Provedores sys, gpu e docker compartilhados
- [x] Mini-apps Docker e Sinais vitais
- [x] Mascotes e temas portados do CyberDock
- [x] Bandeja com a mascote no humor do hub, notificações

**Fase 2** — plugins externos sem recompilar:
- Views em `.slint` carregadas pelo `slint-interpreter` a partir de `~/.local/share/cyberhub/miniapps/`
- Lógica em Rhai, com acesso só aos serviços do host (executor com allowlist, config, notify)
- Widget flutuante na área de trabalho (layer-shell no KDE/wlroots; janela comum no GNOME)

**Fase 3 (talvez)** — expor o hub por D-Bus para cascas nativas (plasmoid, extensão GNOME).

## Decisões e por quês

| Decisão | Por quê |
|---|---|
| Rust + Slint | Leve para ficar sempre ligado; `.slint` é declarativo como o QML do CyberDock |
| `docker` CLI em vez de `bollard` | Paridade com o CyberDock, menos dependências (sem hyper/tokio), passa pela allowlist |
| Threads em vez de tokio | Três provedores com polling não justificam um runtime async |
| Mascote desenhada em Rust | Mesma pixel art e regras de cor do CyberDock, nítida em qualquer escala (`image-rendering: pixelated`) |
| Mini-apps compilados na fase 1 | Contrato de plugin nasce de apps reais (princípio 6) |
| Renderizador por software do Slint | Com o femtovg (OpenGL) o processo carregava o driver da GPU: ~270 MB de RSS. Por software: ~30 MB, e a UI em pixel art não precisa de GPU. Sem `drop-shadow` (não existe nesse renderizador): auras são anéis concêntricos |
| Popup de bandeja via X11/XWayland | O Wayland não deixa o app escolher onde a janela abre. Com `$DISPLAY` o hub roda como cliente X11 (XWayland no KDE/GNOME, nativo no XFCE): abre junto do ícone clicado, sem moldura, fora da barra de tarefas (`_NET_WM_STATE_SKIP_TASKBAR`) e some ao perder o foco ou com Esc. `CYBERHUB_NATIVE_WAYLAND=1` força Wayland nativo (sem posição) |
| `ksni` para a bandeja | StatusNotifierItem puro em Rust (D-Bus); funciona em KDE, XFCE, Cinnamon, MATE e GNOME com AppIndicator |
| Licença do Slint | Usado sob a *Royalty-free Desktop License* (exige crédito ao Slint no "Sobre"); alternativa: GPLv3 |
