# MiniUtils — arquitetura

MiniUtils é um **hub de mini-apps** para o desktop (Linux e Windows): um único binário Rust que fica na
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
   A exceção é declarada: o app externo (`[app]`) tem acesso total, e a loja o anuncia
   assim, com o hash do executável conferido (ver "Plugins e loja").
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
   (no Windows: sysinfo · nvidia-smi · docker CLI · controles de mídia do sistema)
                                                                    * = fase 2
```

## Estrutura de pastas

```
hub-app-rust/
  Cargo.toml                  pacote `miniutils` e o workspace (com ui/)
  AGENTS.md                   regras para agentes de código (compilar, validar, UI)
  ARCHITECTURE.md             este documento
  DESIGN.md                   visuais, temas, telas, apps e mascotes: como criar
  install.sh, install.ps1     instalação em uma linha (Linux, Windows) a partir do último Release
  tools/sprite-import/        converte PNG (folha de quadros 16×14) em animação de mascote,
                              pela paleta cor -> papel; `cargo run -p sprite-import -- --help`
  examples/                   plugins de exemplo (plugin-uptime, plugin-top), app externo de
                              exemplo (app-hello, em Rust) e a loja de teste (loja/preparar.sh)
  ui/                         crate `miniutils-ui`: só a interface, compilada à parte
    Cargo.toml, build.rs      compila hub.slint (e tudo que ele importa, inclusive as telas
                              dos mini-apps); src/lib.rs reexporta os tipos gerados
    hub.slint                 janela (HubWindow): junta as partes abaixo e exporta o que o Rust usa
    models.slint              structs que o Rust preenche (GridCell, AppInfo, PageInfo…)
    dashboard.slint           painel: grade dos mini-apps e modo de edição
    settings.slint            configurações (painel, aparência, sistema, páginas, apps)
    nav.slint                 navegação entre páginas: barra lateral ou abas no cabeçalho
    frame.slint               moldura da janela sem borda: alças, fundo, barras, título
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
      mod.rs                  estado, montagem, `update` (acordado pelo barramento),
                              quadro de animação, bandeja
      window.rs               popup: abrir, esconder, fixar, fade, tamanho lembrado
      appearance.rs           visual, fundo, ícone da bandeja, movimento, relógio
      pages.rs                páginas: barra lateral, página aberta, edição
      grid.rs                 grade do painel e modo de edição (mover, redimensionar)
      settings.rs             Configurações: lista de apps e formulário do manifesto
      bind.rs                 callbacks da tela e eventos da janela
    instance.rs, instance/    instância única (a 2ª só pede para abrir): socket Unix no Linux,
                              named pipe no Windows
    ui.rs                     tipos gerados do Slint
    tray.rs, tray/            ícone na bandeja com a mascote do app em pior estado: SNI (ksni)
                              no Linux, área de notificação (tray-icon) no Windows
    popup.rs                  popup de bandeja: posição, fora da barra de tarefas, fade;
                              X11/XWayland no Linux, Win32 no Windows
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
      os.rs                   miudezas por sistema: encerrar processo, arquivo só do dono,
                              pasta de dados XDG, pasta dos registros
      logging.rs              registro em arquivo + saída de erro (crate `log`) e pânico
                              registrado com a pilha; ver "Registro" abaixo
      about.rs                versão (`--version`, registro e Configurações › Sistema)
      bus.rs                  provedores em threads (`Topic`), assinaturas, "poke" para
                              ler já, janela visível ou escondida
      wake.rs                 despertador da thread da UI: leitura ou recado novo agenda
                              uma atualização (sem timer de varredura)
    providers/
      mod.rs                  `spawn_all`: registra todos os provedores no barramento
      sys.rs, sys/            (/proc e /sys no Linux; crate sysinfo no Windows)
                              CPU (uso, GHz, carga), RAM e swap, temperatura, rede
                              (interface, IP), partições e leitura/escrita dos discos
      gpu.rs                  NVIDIA por um `nvidia-smi -l` de vida longa; senão amdgpu via sysfs
      docker.rs               docker ps / stats / events
      media.rs, media/        o que está tocando e comandos do player: MPRIS pelo D-Bus no
                              Linux, controles de mídia do sistema no Windows
      ai_usage.rs             uso das assinaturas de IA (Codex, Claude, Kiro, OpenCode Go)
      ports.rs                portas escutando: `ss -lntup` + /proc/<pid>/cmdline no Linux,
                              `netstat -ano` + sysinfo no Windows
      procs.rs, procs/        processos mais pesados (por CPU e por memória), só para
                              plugins (`uses = ["procs"]`): /proc no Linux, sysinfo no
                              Windows. Existe para plugin nenhum depender do `ps`
      agents/                 sessões dos codificadores de IA e o estado de cada uma, lido dos
                              arquivos que as ferramentas já gravam (só leitura). Uma fonte por
                              ferramenta (`claude.rs`, `codex.rs`, `opencode.rs`) atrás do trait
                              `Source`; o que cada uma sabe a mais vai em `Details` (opcional)
    skin/
      mod.rs                  visuais da janela (tabela SKINS) e `apply` para o global Skin
      backdrop.rs             fundos desenhados em Rust (pontos, CRT, papel, cidade)
    mascot/
      sprites.rs              sprites: 12 por papéis de cor (16×14, inclui o CD) + 20 "pixel" (12×12)
      themes.rs               temas de cor dos apps
      render.rs               sprite + tema + humor + quadro -> imagem RGBA 16×14
      anim.rs                 piscada e reações curtas (`Animator`, `Reaction`)
      clips.rs, clips/        animações quadro a quadro por mascote (`<chave>.toml`): humor
                              com entrada/saída/variações, reação, duração por quadro;
                              `MINIUTILS_CLIPS=<pasta>` lê do disco e recarrega ao salvar
      preview.rs              `miniutils --previa <mascote> [animação]`: GIF em todos os temas
      disc.rs                 o CD 24×24 do app de música (desenho próprio, não é mascote)
    apps/
      kit.rs                  peças comuns: mascote do cabeçalho (`HeaderMascot`), fala
                              com prazo (`Speech`), `plural`, `unix_now`
      docker.rs  vitals.rs    lógica de cada mini-app
      cdplayer.rs             CD Player: faixa, progresso, comandos e humor
      ai_usage.rs             Assinaturas de IA: um cartão por conta, barras de uso
      ai_pace.rs              previsão de esgotamento pelo ritmo de uso (duas janelas) e gráfico da última hora
      ports.rs                Portas abertas: lista do `ss`, abrir no navegador, encerrar
      todo.rs                 To-do rápido: lista salva em ~/.config/miniutils/todo.json
      agents.rs               Agentes de IA: quem trabalha, quem espera o usuário, quem terminou
      plugin/                 plugins da comunidade: tela genérica, tela interpretada + Rhai,
                              ou tela interpretada + processo do app externo
        mod.rs                conjunto de plugins carregados e o mini-app; API do script no topo
        view.rs               tela interpretada (slint-interpreter): imports e imagens permitidos
        script.rs             o main.rhai numa thread própria: API, allowlist, HTTP, dados
      external.rs             app externo: protocolo JSON por linha e o processo (iniciar,
                              ler, escrever, encerrar o grupo)
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
  são os do enum `Topic` (`sys`, `gpu`, `docker`, `media`, `ai_usage`, `ports`, `agents`, `procs`): um nome desconhecido
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
    fn window_shown(&mut self, ui: &HubWindow) {}        // a janela abriu (ex.: pedir leitura nova)
    fn tick(&mut self, frame: u32, ui: &HubWindow) {}    // quadro (4 a 8/s), janela aberta na página do app
    fn settings_changed(&mut self, ui: &HubWindow) {}    // mudou configuração, tema ou visual
    fn mood(&self) -> Mood;                              // sleep < idle < happy < busy < alarm
    fn mascot_key(&self) -> String { /* icon */ }        // mascote na bandeja
    fn summary(&self) -> String;                         // linha do tooltip da bandeja
    fn tray_media(&self) -> Option<TrayMedia> { None }   // faixa e controles no menu da bandeja
    fn tray_command(&mut self, verb: &str, ui: &HubWindow) {} // controle de mídia escolhido no menu
    fn take_facts(&mut self) -> Vec<Fact> { vec![] }     // conversa entre mascotes (ver abaixo)
    fn voices(&self) -> Vec<Voice> { vec![] }
    fn lines(&self) -> &'static str { "" }
}
```

O hub não conhece nenhum app: o que um app precisa quando a janela abre ou quando o menu
da bandeja manda um comando chega pelo trait.

O `Host` oferece `config`, `bus`, `notifier`, `theme_for(manifest)` (resolve "auto" pelo
visual), `exec_for(manifest)` (executor com a allowlist do app), `reporter(manifest)` e
`set_setting`/`toggle_setting` (muda uma opção do app pela própria tela, grava e redesenha
as Configurações).

O que os apps repetiam está em `src/apps/kit.rs`: `HeaderMascot` (piscada, reações,
cutucada e cache do quadro da mascote do cabeçalho) e `Speech` (fala com prazo; chame
`take_expired` no `tick` para ela sumir na hora certa).

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
provedor (thread) ──snapshot──► canal ──► Waker ──► thread da UI: Hub::update
                                                         │
                                  para cada app ativo que usa o provedor:
                                      app.on_event() → globals/modelos do Slint
                                  depois: humor das páginas, bandeja, conversa
```

- Nada na thread da UI roda por varredura. Cada leitura de provedor e cada recado de ação
  em segundo plano toca o despertador (`core/wake.rs`), que agenda um `Hub::update` no event
  loop; vários toques antes do hub atender viram uma atualização. Com a janela aberta, o
  quadro de animação também confere humor, bandeja e conversa (é onde chegam os efeitos
  dos cliques nas telas). Com a janela escondida, só os eventos acordam o hub.
- Provedores rodam em threads próprias (`bus.spawn` / `spawn_polled`). Sem assinante, a
  thread dorme numa `Condvar` até alguém assinar; com assinante, até o próximo intervalo ou
  um `poke` (leitura imediata, ex.: depois de parar um container).
- O Docker é por eventos (`docker events`, com o filtro de ações aplicado no daemon), com
  `ps` de reserva a cada 15 s e `stats` a cada 5 s só com containers rodando. Com a janela
  escondida os dados só alimentam a bandeja: `ps` a cada 60 s e `stats` a cada 30 s; ao abrir
  a janela, o app pede uma leitura completa (`MiniApp::window_shown`; `Bus::set_visible`,
  `Poker::visible`).
- Nenhuma chamada externa espera para sempre: comandos têm prazo (`exec::run_timeout` mata o
  grupo de processos ao estourar) e as chamadas D-Bus do MPRIS têm `method_timeout`. Um
  daemon ou player travado vira erro na tela, não uma thread presa mostrando dado velho.
- Programa ausente não é falha: sem `docker` no PATH o app mostra "não instalado" e dorme,
  em vez de pôr a bandeja em alarme. `nvidia-smi` presente mas sem funcionar cai para o amdgpu.
- A NVIDIA é lida de um único `nvidia-smi -l 2` de vida longa (uma thread guarda a linha mais
  nova), encerrado quando ninguém pede leitura há 10 s.
- A bandeja só recebe o que muda (`sync_tray`): o ícone é redesenhado quando a mascote, o
  tema ou o humor mudam; texto que só muda de valor ("CPU 37%") vai no máximo a cada 15 s
  (um timer de uma vez manda o texto adiado, mesmo sem leitura nova).
- A UI só é tocada na thread do event loop do Slint.
- O quadro de animação (mascotes, cursor do Terminal, relógio) roda só com a janela
  visível — o timer para ao esconder: 8/s com o mouse sobre a janela, 4/s fora. Flutuar e
  pular são animações do Slint, sem redesenhar.
- Listas do Slint só são recriadas quando o conteúdo muda de fato: recriar uma lista destrói
  os componentes dentro dela (um select aberto fecha). Ver `rebuild_settings`.

### Conversa entre mascotes

```
app ──Fact──► hub (update/quadro) ──► Director ──► global Chatter ──► caixa de diálogo
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
  de) e "digita" uma fala por vez (quadros de 100 ms só enquanto há fala no ar). Quem fala
  dá pulinhos e quem ouve mostra "..." (o `Mascot` compara o próprio `voice`, "app:nome",
  com `Chatter.speaker`/`listener`; o id vem do Rust, `Voice::id_of`).
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

## Registro e iniciar com o sistema

- **Registro** (`core/logging.rs`): o código usa `log::warn!`, `log::info!`… Cada linha
  vai para a saída de erro e para `miniutils.log`, em `~/.local/state/miniutils/` no
  Linux e `%LOCALAPPDATA%\miniutils\logs\` no Windows (a versão final do Windows não tem
  console: sem o arquivo, um erro sumiria). Passando de 1 MiB, vira `miniutils.1.log`.
  Do MiniUtils entra a partir de `info` (`debug` com `MINIUTILS_DEBUG=1` ou
  `--plugin-dev`); das bibliotecas, só avisos e erros. Um pânico em qualquer thread é
  registrado com o lugar e a pilha. Os comandos de terminal (loja, `--help`) seguem com
  `println!`/`eprintln!`, porque a saída é a resposta deles.
- **Iniciar com o sistema** (`src/autostart.rs`, Configurações › Sistema): quem guarda o
  estado é o sistema, não a config. No Linux, `~/.config/autostart/miniutils.desktop`
  (XDG); no Windows, o valor `MiniUtils` em `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
  Os dois abrem com `--hidden`. Ao abrir, se o executável mudou de lugar, a entrada é
  regravada.

## Roteiro

**Fase 1 (atual)** — mini-apps compilados junto, contrato estável:
- [x] Manifesto, registro, ligar/desligar, configuração em TOML com migrações
- [x] Provedores sys, gpu, docker, media, ai_usage e ports compartilhados
- [x] Mini-apps Docker e Sinais vitais (com avisos sustentados)
- [x] Mini-app CD Player (MPRIS: faixa, progresso, comandos, notificação opcional)
- [x] Mini-app Assinaturas de IA (limites e créditos por conta)
- [x] Mini-apps Portas abertas e To-do rápido
- [x] Mini-app Agentes de IA (Claude Code, Codex e OpenCode, só leitura)
- [x] Bandeja com a mascote no humor do hub, notificações, instância única
- [x] Grade com arrastar/redimensionar, visuais da janela, temas claros, 31 mascotes

**Plugins e loja** (feito; detalhes no topo de `src/core/plugins.rs`, `src/apps/plugin/mod.rs`,
`src/apps/external.rs` e `src/store.rs`; guia para quem cria apps em [APPS.md](APPS.md)):
- Três tipos de plugin; os dois primeiros sem código nativo:
  - **simples** (`[run]`): o host roda um comando da allowlist a cada N segundos e mostra a
    saída numa tela genérica (`ui/plugin.slint`);
  - **com tela própria** (`[ui]`): `view.slint` interpretado pelo `slint-interpreter` e
    encaixado na célula por um `ComponentContainer` (recurso experimental do Slint 1.18,
    ligado em `ui/build.rs`); a lógica é um `main.rhai` numa thread só dele, com limite de
    operações, sem acesso a arquivo (`no_module`) e só com a API do host: mudar
    propriedades da tela, humor, rodar programas da allowlist (`run` ou `run_async`, que
    não trava o script), consultar por HTTPS os domínios de `[permissions] net`
    (`http_get`, com prazo, limite de 1 MiB e redirecionamento só para domínio
    permitido), receber as leituras dos provedores de `uses` (`fn on_event(topic, data)`,
    o mesmo barramento dos apps embutidos), guardar dados (`store_get`/`store_set`, um
    JSON por plugin em `plugins/.data/`, gravado pelo host e apagado ao remover), contar
    fatos para a conversa entre mascotes (`fact`) e notificar. A tela importa
    `std-widgets.slint`, `miniutils.slint` (o `Theme`, servido pelo host, com as cores do
    tema do app e as fontes do visual) e as telas de `ui/` do próprio plugin; imagens só de
    `assets/`. O host a encaixa como janela de fundo transparente e com a fonte da
    interface do visual.
- **App externo** (`[app]`, acesso total; `src/apps/external.rs`): para o que um script não
  alcança. A lógica é um executável próprio (qualquer linguagem; o exemplo é Rust), que o host
  inicia ao ligar o app e encerra (com o grupo de processos) ao desligar. Conversam por JSON,
  uma mensagem por linha: o host manda `start`, `tick`, `action`, `event` (provedores de
  `uses`), `settings`, `shown` e `stop`; o app manda `set`, `image`, `mood`, `summary`,
  `notify`, `fact` e `error`. A saída de erro do app vai para o registro. A tela é o mesmo
  `view.slint` interpretado dos plugins com tela, e humor, bandeja e conversa também são
  os mesmos. O app roda com as permissões do usuário, então não declara `[permissions]`. Se
  sair sozinho, o cartão fica em alarme e o botão de atualizar o inicia de novo. O processo
  isola: um app que trava ou cai não derruba o hub. Custa um processo a mais por app
  ligado (exceção ao princípio 1, só para apps externos).
- Desenvolvimento: `miniutils --plugin-dev <pasta>` carrega o plugin direto da pasta (já
  ligado, na primeira página), recarrega ao salvar e mostra no terminal os erros da tela e
  do script.
- Loja na janela (Configurações > Loja de plugins, `ui/store.slint`, `src/hub/store.rs`) e
  na linha de comando (`miniutils --help`): listar, instalar, atualizar e remover sem
  reiniciar (o app aberto relê a pasta; a linha de comando avisa pelo socket da instância).
- O índice (`registry.json`, no modelo do marketplace do Omarchy) fixa o commit revisado de
  cada plugin e é assinado com ed25519 (`registry.json.sig`). Por padrão vale a loja
  oficial (github.com/pedroluizmossi/miniutils-store; índice e chave em
  `store::OFFICIAL_INDEX`/`OFFICIAL_KEY`); a config (`store_index`, `store_key`) troca por
  outra. Índice que não é o oficial, sem chave, só é aceito se for local. A instalação clona sem
  hooks, confere o commit, valida manifesto, tela e script, confere que as permissões
  anunciadas (`permissions`, `net`, `uses` no índice) batem com as pedidas e mostra o que o
  plugin vai executar, consultar e ler (numa atualização,
  as permissões novas aparecem em destaque) antes do segundo clique.
- Na loja, o app externo é anunciado com `full_access` e, por sistema, o endereço e o
  SHA-256 do executável (`binaries`). O executável não vem do repositório: é baixado do
  endereço do índice (o ideal é o CI da loja compilar a partir do commit revisado) e só
  entra se o hash bater. A confirmação mostra "ACESSO TOTAL" em destaque, e uma atualização
  que passa a pedir acesso total aparece como permissão nova. Antes de atualizar ou remover
  pela janela, o hub para o processo (no Windows, um executável rodando trava a pasta).
- Exemplos: `examples/plugin-uptime/` (simples), `examples/plugin-top/` (tela e script),
  `examples/app-hello/` (app externo em Rust; desenvolvimento no README dele) e
  `examples/loja/preparar.sh` (repositório, índice e chave; `--nova-versao` para testar a
  atualização).
- Limites conhecidos: um plugin com tela custa a compilação do `.slint` ao ligar e alguns
  MB de memória; o `tick` é de no mínimo 1 s; o script tem limite de 5 milhões de
  operações por chamada; um plugin não traz falas próprias para a conversa (usa as
  genéricas do hub). Quem lê `agents` e tem rede pode mandar para fora os pedidos feitos
  aos agentes: a loja mostra as duas permissões antes de instalar.

**Fase 2** (resto) — widget flutuante na área de trabalho (layer-shell no KDE/wlroots;
janela comum no GNOME).

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
| Uma implementação por sistema atrás da mesma interface (`linux.rs` / `windows.rs`) | O Linux segue lendo `/proc`, D-Bus e X11 direto (leve e já testado); o Windows ganha o equivalente nativo (`sysinfo`, `tray-icon`, Win32/WinRT pelo crate `windows`) sem camada genérica no meio. As regras estão em "Plataformas" no AGENTS.md |
| Licença do Slint | Usado sob a *Royalty-free Desktop License* (exige crédito ao Slint no "Sobre"); alternativa: GPLv3 |
