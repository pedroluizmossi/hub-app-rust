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
 │                Provedores: sys · gpu · docker · media (MPRIS) · ai_usage       │
 │                           (thread própria, coleta 1×, distribui)               │
 └────────────────────────────────────────────────────────────────────────────────┘
   /proc + /sys + hwmon     nvidia-smi / amdgpu      docker CLI      D-Bus (MPRIS)
                                                                    * = fase 2
```

## Estrutura de pastas

```
hub-app-rust/
  Cargo.toml                  pacote `miniutils` e o workspace (com ui/)
  AGENTS.md                   regras para agentes de código (compilar, validar, UI)
  ARCHITECTURE.md             este documento
  DESIGN.md                   visuais, temas, telas, apps e mascotes: como criar
  ui/                         crate `miniutils-ui`: só a interface, compilada à parte
    Cargo.toml, build.rs      compila hub.slint (e tudo que ele importa, inclusive as telas
                              dos mini-apps); src/lib.rs reexporta os tipos gerados
    hub.slint                 janela: moldura, cabeçalhos, painel (grade + edição), configurações
    theme.slint               tokens: Skin (visual da janela), Colors (tema de app), Icons, Anim
    widgets.slint             componentes: Mascot, Icon, IconButton, TextButton, HeaderButton,
                              SqSwitch, Stepper, Select, SegBar, Sparkline, Counter, SearchBox
    fonts/                    Chakra Petch, JetBrains Mono, Pixelify Sans (+ licenças OFL)
  miniapps/
    <app>/manifest.toml       manifesto (id, provedores, permissões, configurações)
    <app>/view.slint          tela do app (global <App>App + componente <App>View)
    docker/ vitals/ cdplayer/ ai_usage/
  src/
    main.rs                   argumentos (--hidden, --tab) e event loop
    hub/                      o host, um `impl Hub` dividido por assunto:
      mod.rs                  estado, montagem, laço de dados e de animação, bandeja
      window.rs               popup: abrir, esconder, fixar, fade, tamanho lembrado
      appearance.rs           visual, fundo, ícone da bandeja, movimento, relógio
      pages.rs                páginas: barra lateral, página aberta, edição
      grid.rs                 grade do painel e modo de edição (mover, redimensionar)
      settings.rs             Configurações: lista de apps e formulário do manifesto
      bind.rs                 callbacks da tela e eventos da janela
    instance.rs               instância única por socket Unix (a 2ª só pede para abrir)
    ui.rs                     tipos gerados do Slint
    tray.rs                   ícone na bandeja com a mascote do app em pior estado
    popup.rs                  popup de bandeja: posição, fora da barra de tarefas, X11/XWayland
    core/
      miniapp.rs              o trait MiniApp, o Host (serviços) e o Reporter (recado de
                              uma ação em segundo plano para o app que a pediu)
      manifest.rs             formato do manifest.toml
      registry.rs             liga/desliga mini-apps e distribui eventos
      config.rs               ~/.config/miniutils/config.toml (versão, migrações, nome exibido)
      layout.rs               grade do painel: posições, encaixe ao arrastar (settle), vagas
      exec.rs                 executor com allowlist; todo comando tem prazo (`run_timeout`)
                              e erro tipado (`ExecError`: negado, ausente, prazo, falhou)
      notify.rs               notificações (no máximo 1/min por evento)
      bus.rs                  provedores em threads (`Topic`), assinaturas, "poke" para
                              ler já, janela visível ou escondida
    providers/
      mod.rs                  `spawn_all`: registra todos os provedores no barramento
      sys.rs                  CPU (uso, GHz, carga), RAM e swap, temperatura, rede
                              (interface, IP), partições e leitura/escrita dos discos
      gpu.rs                  NVIDIA por um `nvidia-smi -l` de vida longa; senão amdgpu via sysfs
      docker.rs               docker ps / stats / events
      media.rs                MPRIS pelo D-Bus: o que está tocando e comandos do player
      ai_usage.rs             uso das assinaturas de IA (Codex, Claude, Kiro, OpenCode Go)
      ports.rs                portas escutando, pelo `ss -lntup` + /proc/<pid>/cmdline
    skin/
      mod.rs                  visuais da janela (tabela SKINS) e `apply` para o global Skin
      backdrop.rs             fundos desenhados em Rust (pontos, CRT, papel, cidade)
    mascot/
      sprites.rs              sprites: 12 por papéis de cor (16×14, inclui o CD) + 20 "pixel" (12×12)
      themes.rs               temas de cor dos apps
      render.rs               sprite + tema + humor + quadro -> imagem RGBA 16×14
      anim.rs                 piscada e reações curtas (`Animator`, `Reaction`)
      disc.rs                 o CD 24×24 do app de música (desenho próprio, não é mascote)
    apps/
      docker.rs  vitals.rs    lógica de cada mini-app
      cdplayer.rs             CD Player: faixa, progresso, comandos e humor
      ai_usage.rs             Assinaturas de IA: um cartão por conta, barras de uso
      ai_pace.rs              previsão de esgotamento pelo ritmo de uso (duas janelas) e gráfico da última hora
      ports.rs                Portas abertas: lista do `ss`, abrir no navegador, encerrar
      todo.rs                 To-do rápido: lista salva em ~/.config/miniutils/todo.json
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

- `uses` liga os provedores quando o app é ativado e os solta quando é desativado. Os nomes
  são os do enum `Topic` (`sys`, `gpu`, `docker`, `media`, `ai_usage`): um nome desconhecido
  faz o manifesto falhar no parse, e no Rust o provedor é sempre `Topic::…`, nunca uma string.
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
    fn on_report(&mut self, r: &Report, ui: &HubWindow) {} // uma ação em segundo plano terminou
    fn tick(&mut self, frame: u32, ui: &HubWindow) {}    // quadro (4 a 8/s), janela aberta na página do app
    fn settings_changed(&mut self, ui: &HubWindow) {}    // mudou configuração, tema ou visual
    fn mood(&self) -> Mood;                              // sleep < idle < happy < busy < alarm
    fn mascot_key(&self) -> String { /* icon */ }        // mascote na bandeja
    fn summary(&self) -> String;                         // linha do tooltip da bandeja
    fn tray_media(&self) -> Option<TrayMedia> { None }   // faixa e controles no menu da bandeja
}
```

O `Host` oferece `config`, `bus`, `notifier`, `theme_for(manifest)` (resolve "auto" pelo
visual), `exec_for(manifest)` (executor com a allowlist do app) e `reporter(manifest)`.

**Ações em segundo plano.** Um comando demorado (`docker stop`, um comando do player) roda
numa thread, que leva um `Reporter` e no fim deixa um recado: `done()` ou `error(msg)`. O hub
entrega o recado na thread da UI por `on_report`, e o app o guarda no próprio estado. A thread
nunca escreve na tela: o que ela escrevesse seria apagado pela leitura seguinte do provedor.

Reações das mascotes (`src/mascot/anim.rs`): o `Mood` é o estado que dura; uma reação
(`Celebrate`, `Surprised`) é um pico curto por cima dele, medido pelo relógio (não pelo
quadro). Cada mascote tem um `Animator`, que também defasa a piscada para mascotes iguais não
piscarem juntas. Um alerta interrompe a reação; reações têm prioridade e intervalo mínimo.
O Docker comemora quando um container volta; três cliques rápidos numa mascote disparam o
easter egg (nunca durante um alerta). `reduce_motion` na config desliga movimento e reações.
A bandeja não mostra reações, só o humor.

A bandeja mostra a mascote e o tema do app em **pior** humor, a menos que a config fixe
outra mascote (`tray_icon`) ou outro tema (`tray_theme`). O humor continua vindo dos apps.

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
- O Docker é por eventos (`docker events`, com o filtro de ações aplicado no daemon), com
  `ps` de reserva a cada 15 s e `stats` a cada 5 s só com containers rodando. Com a janela
  escondida os dados só alimentam a bandeja: `ps` a cada 60 s e `stats` a cada 30 s; ao abrir
  a janela, o hub pede uma leitura completa (`Bus::set_visible`, `Poker::visible`).
- Nenhuma chamada externa espera para sempre: comandos têm prazo (`exec::run_timeout` mata o
  grupo de processos ao estourar) e as chamadas D-Bus do MPRIS têm `method_timeout`. Um
  daemon ou player travado vira erro na tela, não uma thread presa mostrando dado velho.
- Programa ausente não é falha: sem `docker` no PATH o app mostra "não instalado" e dorme,
  em vez de pôr a bandeja em alarme. `nvidia-smi` presente mas sem funcionar cai para o amdgpu.
- A NVIDIA é lida de um único `nvidia-smi -l 2` de vida longa (uma thread guarda a linha mais
  nova), encerrado quando ninguém pede leitura há 10 s.
- A bandeja só recebe o que muda (`sync_tray`): o ícone é redesenhado quando a mascote, o
  tema ou o humor mudam; texto que só muda de valor ("CPU 37%") vai no máximo a cada 15 s.
- A UI só é tocada na thread do event loop do Slint.
- O quadro de animação (mascotes, cursor do Terminal) roda só com a janela visível: 8/s com
  o mouse sobre a janela, 4/s fora. Flutuar e pular são animações do Slint, sem redesenhar.
- Listas do Slint só são recriadas quando o conteúdo muda de fato: recriar uma lista destrói
  os componentes dentro dela (um select aberto fecha). Ver `rebuild_settings`.

### Conversa entre mascotes

```
app ──Fact──► hub (timer de 100 ms) ──► Director ──► global Chatter ──► caixa de diálogo
     (take_facts)    voices() da página aberta        (src/hub/chatter.rs)  + pulinho/"..." no Mascot
```

- Um app não fala com outro. Ele registra fatos sobre si (`core/chatter.rs`: `Trouble`,
  `Strain`, `Forecast`, `Recovered`, `Poked`), diz quais mascotes tem na tela (`MiniApp::voices`) e
  traz as próprias falas (`MiniApp::lines`, o `miniapps/<app>/lines.toml`). Mudança de humor
  vira fato com `chatter::mood_fact`. Os três métodos têm padrão vazio: app que não
  implementa fica fora da conversa, e app sem `lines.toml` usa as falas genéricas
  (`src/hub/lines.toml`).
- `lines.toml`: o que a mascote diz em cada caso (`[trouble]`, `[strain]`, `[forecast]`,
  `[recovered]`, `[poked]`, `[idle]`), o que os outros respondem (`[trouble.answer]`) e o que ela responde
  aos outros (`[reply.strain]`, com `about` para limitar o assunto). Formato e lacunas no
  topo de `core/chatter.rs`; um teste confere pt e en em tudo, lacunas e tamanho.
- O diretor não conhece nenhum app: escolhe quem responde (primeiro quem tem resposta
  própria para o caso, depois outro app), sorteia o idioma da conversa (pt, en, às vezes
  de) e "digita" uma fala por vez. Quem fala dá pulinhos e quem ouve mostra "..." (o
  `Mascot` compara o próprio `voice`, "app:nome", com `Chatter.speaker`/`listener`).
- Só com a janela aberta no painel e a opção "Mascotes conversam" (`chatter`: nunca, às
  vezes, bastante) ligada. Quem responde é da página aberta; alarme ou recuperação de outra
  página aparece no app de quem responde, com o nome da página. Com tudo calmo por um
  tempo, sai um papo à toa (`[idle]`).
- Contra falatório: intervalo entre conversas puxadas por fatos (depende da opção), assunto
  repetido só depois de 10 min, cutucada com intervalo curto próprio. Alarme fura o
  intervalo. Fato que não dá para mostrar é descartado.

## Janela, grade e visual

- **Telas:** `current` = `"panel"` (painel) ou `"apps"` (configurações). O modo de edição é
  um estado do painel (`editing`). Esc volta das configurações, conclui a edição ou fecha o popup.
- **Páginas:** a barra lateral (`Rail`) lista as páginas do painel; cada uma tem nome,
  mascote (no humor do pior app dela, com marca de alerta), a própria grade e os seus apps.
  Um app fica numa página só. Embaixo da barra: nova página e configurações. Nas
  Configurações, o cartão Páginas renomeia, troca o ícone, reordena e apaga (os apps vão
  para a página anterior); cada app tem um seletor "Página". Só os apps da página aberta animam.
  Com `nav = "top"` (Aparência › Páginas), as páginas viram abas no cabeçalho (`TopTabs`) no
  lugar da barra lateral, e a engrenagem volta para o cabeçalho.
- **Grade:** colunas × linhas (até 6 × 6) por página; cada app guarda `x`, `y`, `w`, `h` na config.
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
A gravação é atômica (temporário ao lado + `rename`): uma queda no meio deixa o arquivo
anterior intacto. Arquivo vazio conta como ausente, não como versão 0 a migrar.

| Versão | Mudança |
|---|---|
| 0 → 1 | `theme` global vira tema de cada app |
| 1 → 2 | a grade passa a guardar posição (`x`, `y`) e ganha `rows` |
| 2 → 3 | o CD Player entra ligado |
| 3 → 4 | `columns` e `rows` viram a página "Geral" em `[[pages]]`, com os apps ligados |

Campos sem migração (têm padrão): `name` (nome exibido, gravado ao abrir se faltar), `skin`,
`nav` (`"side"` ou `"top"`), `reduce_motion`, `tray_icon` e `tray_theme` (`"auto"` ou a chave de uma mascote/tema; escolhidos em Aparência).
Na primeira execução, a config de `~/.config/cyberhub/` (nome antigo) é copiada.

## Roteiro

**Fase 1 (atual)** — mini-apps compilados junto, contrato estável:
- [x] Manifesto, registro, ligar/desligar, configuração em TOML com migrações
- [x] Provedores sys, gpu, docker, media, ai_usage e ports compartilhados
- [x] Mini-apps Docker e Sinais vitais (com avisos sustentados)
- [x] Mini-app CD Player (MPRIS: faixa, progresso, comandos, notificação opcional)
- [x] Mini-app Assinaturas de IA (limites e créditos por conta)
- [x] Mini-apps Portas abertas e To-do rápido
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
| `nvidia-smi -l` de vida longa | Um processo novo a cada 2 s custava fork, exec e a inicialização da NVML toda vez, inclusive com a janela escondida |
| Prazo em toda chamada externa | Um `docker`, `nvidia-smi` ou player travado prendia a thread do provedor, e a tela seguia mostrando a última leitura como boa |
| MPRIS pelo `zbus` | O `ksni` já traz o zbus; nenhuma dependência nova |
| Threads em vez de tokio | Poucos provedores com polling/eventos não justificam um runtime async |
| Mini-apps compilados na fase 1 | Contrato de plugin nasce de apps reais (princípio 6) |
| Popup de bandeja via X11/XWayland | O Wayland não deixa o app escolher onde a janela abre. Com `$DISPLAY` o hub roda como cliente X11: abre junto do ícone clicado, sem moldura, fora da barra de tarefas e some ao perder o foco ou com Esc. `MINIUTILS_NATIVE_WAYLAND=1` força Wayland nativo (sem posição) |
| Instância única por socket | Abrir de novo (atalho, autostart) só traz a janela da instância que já roda |
| `ksni` para a bandeja | StatusNotifierItem puro em Rust (D-Bus); funciona em KDE, XFCE, Cinnamon, MATE e GNOME com AppIndicator |
| Licença do Slint | Usado sob a *Royalty-free Desktop License* (exige crédito ao Slint no "Sobre"); alternativa: GPLv3 |
