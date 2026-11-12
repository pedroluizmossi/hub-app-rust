# MiniUtils — design

Como o MiniUtils se veste e como criar coisas novas: **visuais** da janela, **temas** de
cor dos apps, **telas**, **mini-apps** e **mascotes** (inclusive de tamanhos diferentes).
O app não tem um estilo só: o cyberpunk é um dos visuais, ao lado de terminal, aconchego,
noite chuvosa e o que vier depois. Arquitetura e fluxo de dados: [ARCHITECTURE.md](ARCHITECTURE.md).

## 1. As três camadas

| Camada | Quem decide | Onde mora | Muda o quê |
|---|---|---|---|
| **Visual da janela** (skin) | o usuário, em Configurações › Aparência | `src/skin/mod.rs` → global `Skin` | moldura, fundo, cabeçalho, botões, fontes, cores da interface |
| **Tema do app** | cada app tem o seu (opção "Tema") | `src/mascot/themes.rs` → struct `Colors` | tudo dentro do cartão do app e as cores da mascote |
| **Mascote** | cada app (opções "Mascote…") | `src/mascot/sprites.rs` | o bichinho e como ele reage ao humor |

Regras que mantêm as camadas separadas:

- A **moldura** (cabeçalhos, configurações, modo de edição) usa só `Skin.*`.
- O **conteúdo de um app** (`miniapps/<app>/view.slint`) usa só o `Colors` do app e as
  fontes do visual (`Skin.font-ui`, `Skin.font-mono`). Nunca `Skin.ink` e afins: o tema do
  app pode ser claro numa janela escura, ou o contrário.
- **Nenhuma cor solta** (`#RRGGBB`) em `.slint`. Se faltar um token, crie o token.
- O tema **"auto"** ("Acompanha a janela") usa o `app_theme` do visual, e é o padrão de todo
  app novo. Assim, trocar o visual troca tudo junto, e quem quiser fixa um tema por app.

## 2. Visuais da janela (skins)

Um visual é uma entrada na tabela `BUILTIN` (`src/skin/mod.rs`) ou um arquivo
`visuais/<chave>.toml` da pessoa (ver [Temas, visuais e fundos por arquivo](#temas-visuais-e-fundos-por-arquivo)).
Nada de layout muda. Por cima do visual, a pessoa escolhe em Configurações › Aparência:

- **Cor de destaque** (`skin::ACCENTS`): troca `accent` e `accent_ink`. `skin::effective`
  clareia ou escurece a cor até 3:1 sobre `bg` e `surface`, então qualquer destaque fica
  legível em qualquer visual. O fundo do painel acompanha (as camadas usam `accent`).
- **Tamanho do texto** (90 a 125%): `Skin.text-scale`. Ver [Tamanho do texto](#tamanho-do-texto).
- **Fundo animado** (liga/desliga; "Reduzir movimento" também desliga).

### Criar um visual

1. Copie a entrada mais parecida em `BUILTIN` e troque `key` (sem espaços, vai para a config),
   `label` (aparece no seletor) e `app_theme` (tema dos apps em "auto"; precisa existir em
   `themes.rs`, um teste confere).
2. Preencha os campos (tabela abaixo). As cores do modo de edição (`edit-*`) e a borda do
   botão de perigo são derivadas do acento por `apply`; não precisa defini-las.
3. Escolha um **fundo** (`backdrop`, pelo nome: `neon`, `terminal`, `papel`, `cidade`). Os
   fundos pintam com as cores do visual, então o mesmo fundo serve a visuais diferentes.
4. Se usar fonte nova, veja [Fontes](#fontes).
5. Confira as **regras de contraste** e rode o app com o visual (veja [Testar](#7-testar-na-tela)).

| Grupo | Campos | Para quê |
|---|---|---|
| Superfícies | `bg` `panel` `panel_top` `edge` `edge_inner` `line` | fundo do conteúdo; cabeçalho/rodapé; brilho no alto do cabeçalho; borda da janela; filete por dentro da borda; linha sob o cabeçalho |
| Blocos e campos | `surface` `surface_edge` `field` `field_edge` `slot` | blocos das configurações; selects/steppers/switches; células vazias no modo de edição |
| Texto | `ink` `sub` `dim` | principal, secundário, apagado (sobre `bg`/`surface`) |
| Texto no cabeçalho | `panel_ink` `panel_sub` `tag_ink` | título, botões e etiqueta ao lado do título (sobre `panel`) |
| Acentos | `accent` `accent_ink` `accent2` `hover` `danger` `bad` | ação principal e foco; texto sobre o acento; alfinete ligado; hover; sair; "sem espaço" |
| Forma | `chamfer` `border` `header_h` `header_line` `panel_top_h` `footer_h` `card_alpha` | canto cortado (0 = reto); espessura da borda; cabeçalho; rodapé (0 = sem; <20 px = só faixa; ≥20 px = barra de status); opacidade dos cartões sobre o fundo |
| Letras | `font_ui` `font_mono` `font_title` | texto geral, números/código, título |
| Cabeçalho | `title` (`Upper`/`Plain`/`Prompt`) `title_size` `title_weight` `title_spacing` `title_cursor` `title_icon` `title_icon_cut` `tag` (`Grid`/`Greeting`/`None`) `buttons` (`Icon`/`Bracket`/`Chunky`) | como o nome aparece (`MINIUTILS`, `MiniUtils`, `miniutils@bandeja:~$`), cursor piscando, ícone em pixel 8×8, o que vai ao lado do título, estilo dos botões |

Os 8 visuais embutidos servem de referência:

| Visual | Fundo | Ideia | Traços |
|---|---|---|---|
| Neon | `neon` | cyberpunk | cantos cortados, grade de pontos com estrelas que piscam, Chakra Petch, botões com ícone |
| Terminal | `terminal` | fósforo verde | tudo mono, prompt com cursor, `[ fixar ]`, barra de status, linhas e faixa de varredura |
| Aconchego | `papel` | casinha de madeira | tema **claro**, borda de 6 px, papel pontilhado, Pixelify, botões "chunky" com sombra |
| Noite chuvosa | `cidade` | cidade à noite | lua, prédios com janelas que apagam, chuva caindo, saudação com hora, cartões translúcidos |
| Synthwave | `neon` | anos 80 | laranja e rosa sobre roxo, cantos cortados menores |
| Gelo | `terminal` | terminal azul | o Terminal em azul gelo, com o tema Ghost |
| Vaporwave | `papel` | pastel | pontilhado lilás, borda grossa, título em Pixelify, botões "chunky" |
| Akira | `cidade` | Neo-Tóquio | a cidade em vermelho, cartões translúcidos |

### Fundos

Um fundo (`src/skin/backdrop.rs`) é uma lista de **camadas** pintadas em ordem sobre `bg`,
numa imagem do tamanho da área do painel, refeita quando o visual, o tamanho ou a escala
mudam. As cores das camadas são cores do visual pelo nome (`accent`, `accent2`, `sub`,
`surface_edge`…) ou fixas (`#RRGGBB`): por isso um fundo serve a vários visuais e segue a
cor de destaque.

| Camada | Desenha | Campos |
|---|---|---|
| `dots` | grade de pontos | `step`, `offset`, `radius`, `color`, `alpha` |
| `glow` | brilho elíptico | `x`, `y` (frações da área), `dx`, `dy` (px), `rx`, `ry`, `fade`, `color`, `alpha` |
| `vignette` | brilho que alcança os cantos | `x`, `y`, `fade`, `color`, `alpha` |
| `stripes` | linhas repetidas num ângulo | `deg`, `step`, `color`, `alpha` |
| `rect` | retângulo | `x`, `y` (frações), `dx`, `dy`, `w`, `h` (px), `color`, `alpha` |
| `rain` | **animada**: gotas caindo | `count` (numa área de 1000×600; escala com a área), `color`, `alpha`, `speed` (px/s), `length` |
| `twinkle` | **animada**: pontos que acendem | `count`, `color`, `alpha`, `size`, `grid` (prende à grade de um `dots`) |
| `scan` | **animada**: faixa de varredura | `color`, `alpha`, `height`, `speed` |

A cidade (`city`) é uma camada só embutida (prédios do design, lua, janelas que apagam).

**Animação.** As camadas animadas não entram na imagem: viram elementos pequenos
(`Ambient`) que `ui/ambient.slint` move a cada quadro pelo relógio `Anim.time`. O
renderizador por software repinta só a região de cada elemento; redesenhar a imagem
inteira custaria de 20 a 100 ms por quadro (medido em 900×560, 1× e 2×). Por isso:

- nada de animar camadas grandes; uma faixa (`scan`) ou dezenas de gotas, não centenas
  (`count` vai até 200);
- cor com alfa no `background`, nunca `opacity` (que pinta numa camada à parte);
- o fundo animado para com a janela escondida (o quadro não roda), com "Reduzir movimento"
  e com o interruptor "Fundo animado".

Regras:

- Desenhe por cima de `bg`; o fundo não pode brigar com o texto dos apps (alfa baixo: 0,05–0,15).
- Elementos presos a um canto (lua, prédios) devem funcionar de 400 px a 2000 px de largura.
- O teste `todos_desenham_algo_alem_do_fundo` pega um fundo que saiu liso por engano, e
  `limites_das_camadas` confere os limites de cada camada embutida.

### Temas, visuais e fundos por arquivo

Quem usa pode criar os três sem recompilar, em `<dados>/miniutils/` (`miniutils --pastas`
mostra o caminho; `MINIUTILS_APARENCIA` troca a pasta):

| Pasta | Arquivo | Começa de |
|---|---|---|
| `temas/` | `<chave>.toml` com `label`, `base` (um tema; padrão `neon`) e as cores de `COLOR_FIELDS` (`fur`, `visor`… ver [Tema de um app](#tema-de-um-app-colors)) em `"#RRGGBB"` | o tema `base` |
| `visuais/` | `<chave>.toml` com `label`, `base` (um visual; padrão `neon`), `app_theme`, `backdrop` e qualquer campo da tabela de [Criar um visual](#criar-um-visual) | o visual `base` |
| `fundos/` | `<chave>.toml` com `label` e `[[layer]]` (de 1 a 32), cada uma com `kind` e os campos da tabela de [Fundos](#fundos) | — |

- A chave é o nome do arquivo: `a-z`, `0-9`, `-`, `_`, até 32. Não pode repetir um embutido.
- Num visual: cores em `"#RRGGBB"` (`panel_top` e `edge_inner` aceitam `"none"`); medidas
  dentro dos limites de `NUM_FIELDS`; fontes só as do binário (`skin::FONTS`); `title`
  `upper`/`plain`/`prompt`; `tag` `grid`/`greeting`/`none`; `buttons`
  `icon`/`bracket`/`chunky`.
- Um visual pode citar um tema e um fundo de arquivo; um tema pode partir de outro de arquivo.
- Arquivo com erro fica de fora, com o motivo em Configurações › Aparência (e no
  registro). Depois de mudar, **Recarregar** (ou `miniutils --recarregar`).
- O contraste não é conferido: siga as [regras de contraste](#regras-de-contraste).

Exemplos comentados (um de cada) em [`examples/aparencia/`](examples/aparencia); o teste
`exemplos_carregam_sem_erro` os lê.

### Fontes

1. Coloque o `.ttf` estático (não o variável: o Slint escolheria o peso errado) em
   `ui/fonts/`, junto da licença (`OFL-<Nome>.txt`).
2. Importe no topo de `ui/hub.slint`: `import "fonts/Nome-Regular.ttf";` (um por peso usado).
3. Use o nome da família (`"Pixelify Sans"`) nos campos `font_*` do visual.
4. Mencione no README. Cada família custa ~100–300 KB no binário.

## 3. Cores e contraste

### Tema de um app (`Colors`)

Um tema é uma entrada em `THEMES` (`src/mascot/themes.rs`). Os campos:

| Campo | Na tela | Na mascote |
|---|---|---|
| `bg0`, `bg1` | fundo do cartão (mistura dos dois) e dos campos | — |
| `text`, `sub`, `muted` | texto principal, secundário, bordas e trilhos | — |
| `fur` | acento do app (chips, botão parar, barra de arrasto) | pelo (`P`, `H`, `S`) |
| `visor` | acento secundário (links, botões de ação), cor "normal" dos números | visor (`C`), olho calmo |
| `outline` | texto sobre `fur` | contorno (`K`) |
| `ok`, `warn`, `alarm` | rodando, agitado, alarme | olho e LED por humor |
| `sleep` | dormindo | "z" do sono |
| `ear`, `blush` | — | orelha/detalhe claro (`M`), bochecha (`B`) |

Um tema novo aparece sozinho na opção "Tema" de todos os apps: a lista é gerada
(`SettingDef::live_options`), e os manifestos não repetem as opções do `theme`.

### Regras de contraste

- Texto normal: **4,5:1** sobre o fundo onde ele fica (`text` sobre `bg0`, `ink` sobre
  `bg`/`surface`, `panel_ink` sobre `panel`, `accent_ink` sobre `accent`). Vale também para
  o texto pequeno e apagado (`sub`, `dim`).
- Acentos usados como texto (`visor`, `accent`, números grandes): pelo menos 3:1.
- Estados que precisam ser distinguidos (`ok`/`warn`/`alarm`) diferem também em **claridade**,
  não só no matiz.
- **Tema claro** (ex.: `aconchego`): `text` escuro, `bg0`/`bg1` claros, e `visor`/`ok`
  escuros o bastante para aparecer em papel. Um `visor` claro some no fundo claro.
- Ao terminar, veja o visual e cada tema com o app em calma **e** em alarme.

### Tamanho do texto

Todo `font-size` com valor em px, na moldura (`ui/`) e nos apps embutidos
(`miniapps/*/view.slint`), é multiplicado por `Skin.text-scale` (0,9 a 1,25, de
Configurações › Aparência): `font-size: 13px * Skin.text-scale;`, ou
`(short ? 15px : 19px) * Skin.text-scale` numa escolha. Ficam de fora o título do cabeçalho
(`Skin.title-size`, que tem de caber no `header_h`) e tamanhos tirados da altura do pai. Os
plugins recebem a escala em `theme.text-scale` e decidem se a usam.

Com texto maior, uma linha pode quebrar e ficar mais alta. Por isso, altura mínima de
linha é `min-height: max(44px, self.preferred-height)` num layout (ou `min-height: 44px` no
`Text` do rótulo), **nunca** `min-height: 44px` no layout: fixo, ele vira a altura da
linha e o texto que quebra invade a de baixo. Confira as telas em 125% com o `dev-ui.sh`.

## 4. Telas da janela

Hoje há duas telas (`current` em `ui/hub.slint`): `"panel"` (painel) e `"apps"`
(configurações). O modo de edição é um estado do painel (`editing`), não uma tela.

Para criar uma tela (ex.: "Sobre"):

1. Escolha um nome e trate-o em `current`: um cabeçalho `Bar { … }` e o conteúdo, com
   `if current == "<nome>": …`. O cabeçalho usa `Title`/`HeaderButton`, para seguir o visual.
2. Dê um caminho de volta: botão "Fechar" e **Esc** (no `FocusScope` da janela).
3. Um app novo, ao ser ligado, entra na página aberta; o usuário muda de página nas
   Configurações. `--tab <nome>` abre uma página pelo nome (ver `Hub::open_tab`).
4. Rodapé, fundo e moldura vêm do visual automaticamente; não desenhe os seus.
5. Conteúdo longo vai num `ScrollView` com `VerticalLayout { alignment: start; … }`.
   Blocos com muitas opções começam **recolhidos** (veja o acordeão das configurações).

### Janela separada (detalhe de um app)

Quando um app precisa mostrar mais do que cabe na célula, ele abre uma janela própria,
fora do popup (aparece na barra de tarefas). Exemplo: o detalhe de uma sessão em
`miniapps/agents/detail.slint`, aberto pelo botão "Detalhes" do cartão.

1. Um `export component <X>Window inherits Window` com um global próprio de dados e
   callbacks; importe e exporte os dois em `ui/hub.slint`. Como o hub, sem a moldura do
   sistema (`no-frame`, fundo transparente): `Frame` do visual, `Bar` com uma `TouchArea`
   que move a janela (`popup::drag_move`), um `HeaderButton` de fechar, Esc e
   `ResizeHandles` (`popup::drag_resize`). Conteúdo num cartão afastado 12 px da borda,
   para não invadir o canto cortado.
2. **Cada janela tem os próprios globais**: `Skin`, `Anim` e o global do app do hub não
   valem nela. Aplique o visual com `skin::apply_current(&janela)` ao criar e em
   `settings_changed` (que roda quando o visual muda).
3. O app guarda a janela: cria no primeiro uso, `show()` e `popup::focus` para trazê-la
   para frente; fechar só esconde (`CloseRequestResponse::HideWindow`). Em `disable`,
   esconda. Não segure o `RefCell` do app ao chamar `show()`: o hub perde o foco na hora.
4. Dado caro só enquanto a janela está aberta (ex.: `providers::agents::watch`).

Regras gerais de interface:

- Alvos de clique com ≥ 36 px de altura (44 px quando for o botão principal).
- Use os componentes de `ui/widgets.slint` (`HeaderButton`, `TextButton`, `IconButton`,
  `SqSwitch`, `Stepper`, `Select`) em vez de desenhar botões novos. Eles já seguem o visual
  e têm papel de acessibilidade (`accessible-role`).
- Ícones: traço SVG num quadro 24×24 (`Icons.*`, desenhados com `Icon`); para o estilo
  "chunky", a versão pixel 7×7 (`Icons.px-*`). Ícone sem texto precisa de `label`.

## 5. Mini-apps

Este é o passo a passo de um mini-app **embutido** (Rust dentro do hub). Plugins e apps
externos, que se instalam pela loja sem recompilar o hub, estão em [APPS.md](APPS.md).

Um app novo, do zero:

1. **Manifesto** — `miniapps/<app>/manifest.toml`: `id` (`cyber.<nome>`), `name`,
   `description`, `icon` (uma mascote), `uses` (provedores), `permissions.exec` e
   `settings`. Inclua sempre a opção `theme` com padrão `"auto"`.
2. **Tela** — `miniapps/<app>/view.slint`:
   - um `export global <App>App { in property <Colors> theme; … }` com os dados e callbacks;
   - um `export component <App>View inherits Rectangle` que se adapta ao tamanho recebido.
   - Cores só do `theme` do app; fontes do `Skin` (seção 1).
3. **Lógica** — `src/apps/<app>.rs`: uma struct com `create(host, ui)` e o trait
   `MiniApp`. Preencha a tela em `enable`, `on_event` e `settings_changed` (que também é
   chamado quando o visual muda e o tema "auto" troca). Registre o módulo em
   `src/apps/mod.rs`.
4. **Registro** — em `src/hub/mod.rs`, `registry.add(crate::apps::<app>::<App>Mini::create(…))`.
5. **No painel** — em `ui/dashboard.slint`, importe a view e acrescente no `Dashboard` um
   `if c.id == "cyber.<nome>": <App>View { … }`; em `ui/hub.slint`, importe o global (e as
   structs da tela) e exporte-os no `export { … }`, para o Rust enxergar.
6. **Provedor** (se precisar de dado novo) — `src/providers/<nome>.rs` com um `collect()`
   que devolve um snapshot; um caso novo em `bus::Event`; `bus.spawn("<nome>", intervalo, …)`
   no `Hub::new`. O provedor só roda enquanto algum app ativo o tiver em `uses`.
7. **Conversa entre mascotes** (opcional) — implemente `voices` (as mascotes na tela),
   `take_facts` (o que aconteceu: use `chatter::mood_fact` quando o humor muda e registre
   `Poked` ao cutucar) e `lines` (`include_str!` do `miniapps/<app>/lines.toml`, só com o
   que o app tem de próprio; o resto vem das genéricas). Na view, dê ao `Mascot` o
   `voice` que o Rust preenche com `Voice::id_of(&manifest.id, nome)` (uma propriedade do
   global ou um campo da linha), nunca um texto escrito à mão.
   Formato das falas: topo de `src/core/chatter.rs`.

### Tamanhos

O app recebe o tamanho exato da célula da grade, que vai de ~300 × 160 px (1×1 numa grade
de 3 colunas) até a janela inteira. Por isso:

- Defina limites e esconda ou compacte o que não cabe (ex.: o Docker tira os contadores
  abaixo de 560 px de largura e os botões secundários abaixo de 420 px; os Sinais vitais
  passam de uma linha para 2 colunas e depois para 1).
- O mais importante (o número, o estado) aparece sempre; gráficos e detalhes saem primeiro.
- Texto longo usa `overflow: elide`; nunca deixe estourar a célula.
- Teste nos tamanhos 1×1, 2×1, 1×2 e na janela inteira (modo de edição).

### Humor e avisos

- `mood()` resume o app: `Sleep` (nada acontecendo) < `Idle` < `Happy` < `Busy` < `Alarm`.
  A bandeja mostra o app em pior humor, então só use `Busy`/`Alarm` quando o usuário deve olhar.
- Notificações (`host.notifier`) para o que importa **e dura**: avise uma vez e rearme só
  quando o valor voltar abaixo de uma folga (veja `Watch` em `src/apps/vitals.rs`).

## 6. Mascotes e sprites

Toda mascote é desenhada num **quadro de 16 × 14 pixels** (`W`, `H` em `render.rs`). É esse
quadro que a UI amplia (`Mascot`, proporção 16:14), que a bandeja centraliza num ícone
quadrado e que o cache de quadros compara. Há dois formatos de mascote e um caminho para
desenhos de outros tamanhos.

### Formato 1 — por papéis de cor (16 × 14)

As 11 mascotes originais (neko, robô, kitsune…). Cada letra é um **papel**, e o tema decide
a cor: a mesma mascote muda de cor com o tema do app.

`.` transparente · `K` contorno · `P` cor principal · `S` sombra · `H` brilho ·
`M` detalhe claro · `C` visor · `E` olho · `W` brilho do olho · `B` blush · `T` boca ·
`F` pele · `D` hardware · `A` LED (segue o humor)

- `fur`/`ear` com `Some(cor)` fixam a cor de `P`/`M` (sapo sempre verde, placas de vídeo).
- `eye_bg` é o papel que aparece no lugar do olho quando ele fecha; `shade_from` escurece
  `P` a partir de uma linha.

### Formato 2 — "pixel", com paleta própria (até 16 × 14)

Os 20 bichinhos da Toca. Cores fixas do design, para manter a identidade de cada um.

- `pixel: Some(Pixel { eye, pal })`: `pal` liga cada **letra minúscula** a uma cor;
  `eye` é a cor do olho calmo.
- `.` é transparente e **`e` é o olho**: só ele segue o humor (cor do olho calmo; `warn` do
  tema quando agitado; `alarm` piscando no alarme; fechado dormindo, com o resto apagado).
  Toda mascote "pixel" precisa de pelo menos um `e` (um teste confere).
- O desenho vai **centralizado** no quadro 16 × 14. Um design 12 × 12 ganha 2 colunas de `.`
  de cada lado e 1 linha em cima e embaixo; um 14 × 14 ganha 1 coluna de cada lado, e assim
  por diante.

Para trazer mascotes de um design (como o `Mascotes.dc.html` do artefato), gere o Rust a
partir dos dados (linhas + paleta) com um script, em vez de copiar à mão: o script faz o
enquadramento e confere se toda letra tem cor. Foi assim que os 20 entraram.

### Escolher o tamanho

| O desenho tem… | Faça |
|---|---|
| até 16 × 14 | formato 1 ou 2, centralizado no quadro |
| algo maior, mas é uma **mascote** (aparece no lugar das outras) | redesenhe em 16 × 14: a mascote precisa caber no mesmo quadro que as outras para trocar entre elas, aparecer na bandeja e nas configurações |
| algo maior que **não** é mascote (ilustração, capa, disco) | um módulo de desenho próprio com o tamanho que precisar, como `src/mascot/disc.rs` (o CD 24 × 24 do app de música), mostrado com o próprio `Image` na view do app |
| muitas mascotes maiores (ex.: tudo em 24 × 24) | aumente `W`/`H` em `render.rs` para todas, de uma vez: as atuais ficam centralizadas no quadro maior. Confira `argb_square` (bandeja, supõe `W ≥ H`), a proporção do `Mascot` em `widgets.slint` (`preferred-width/height`) e os ícones das configurações |

Regras para qualquer pixel art:

- Contorno fechado e escuro: a mascote aparece sobre fundos claros e escuros.
- Olho de 1–2 pixels, longe da borda: a animação de alarme desloca as linhas 4–7 em 1 pixel.
- Nada de detalhe de 1 pixel isolado que só funcione numa cor: o humor e o tema mudam cores.
- Flutuar, pular e tremer são do Slint (`Mascot`); piscar, LED e glitch são do Rust. O
  `grid` do sprite é a pose base; poses e movimento desenhados vão nas animações (abaixo).

### Animações (poses quadro a quadro)

Uma mascote pode ter animações em `src/mascot/clips/<chave>.toml` (registre o arquivo em
`SOURCES`, em `src/mascot/clips.rs`). Cada animação é uma sequência de quadros no mesmo
formato do `grid` (14 linhas, mesmos papéis ou letras da paleta). Piscada, LED, olhos do
humor e glitch continuam sendo aplicados por cima de cada quadro.

```toml
[idle]
durations = [1600, 120, 120]   # ms de cada quadro: segura uma pose, mexe rápido
variants = ["coca", "olha"]    # de vez em quando, uma delas no lugar do loop
every = [8, 20]                # a cada 8 a 20 s (sorteado)
exit = "acorda"                # toca uma vez ao sair deste humor
frames = [ ... ]

[sleep]
fps = 1                        # ou quadros por segundo, todos iguais
enter = "boceja"               # toca uma vez ao entrar, antes do loop
frames = [ ... ]

[boceja]                       # transição: toca uma vez (sem loop)
durations = [200, 400, 200]
frames = [ ... ]
```

- Com o nome de um **humor** (`sleep`, `idle`, `happy`, `busy`, `alarm`), toca sempre que a
  mascote está nele (repete por padrão), com `enter`, `exit` e `variants`.
- Com o nome de uma **reação** (`celebrate`, `surprised`), toca enquanto a reação dura.
- Outros nomes são transições e variações (citadas por um humor) ou ficam disponíveis por
  `clips::of(chave)` para quem pedir (um jogo, um palco).
- Com "Reduzir movimento" ligado, a mascote fica na pose base.
- Quadros de até 33 ms (30/s): enquanto uma animação com quadros curtos está na tela, o hub
  acelera o timer de animação, e volta ao ritmo calmo quando ela para.

O olho fechado (piscada, dormindo) é desenhado na última linha de olho **de cada quadro**,
então uma pose pode mexer a cabeça. `cargo test` confere tamanho, cores, durações e nomes
citados de todos os arquivos (`arquivos_de_animacao_validos`).

**Desenhar sem recompilar:** rode com `MINIUTILS_CLIPS=src/mascot/clips cargo run`. O app
lê os `.toml` da pasta (todos, sem precisar registrar em `SOURCES`) e recarrega ao salvar;
erros aparecem no terminal e o arquivo com erro fica de fora até ser corrigido.

**Prévia em GIF:** `cargo run -- --previa neko sleep` grava `neko-sleep.gif` com a animação
em todos os temas, ampliada (sem o nome da animação, todas em sequência; `--saida` escolhe
o arquivo). Funciona junto com `MINIUTILS_CLIPS`.

**Desenhar num editor** (Aseprite, LibreSprite, Piskel): exporte uma folha PNG com quadros
de 16 × 14 lado a lado e converta:

```sh
cargo run -p sprite-import -- sleep.png > paleta.toml        # modelo com as cores do PNG
# troque cada ? em paleta.toml pelo papel da cor (K contorno, P principal, E olho…)
cargo run -p sprite-import -- sleep.png --paleta paleta.toml --fps 2 >> src/mascot/clips/neko.toml
```

A paleta é por cor, então desenhe com uma cor por papel (a cor real vem do tema do app).
Para uma mascote "pixel", use as letras da paleta dela e `e` no olho. Pacotes prontos (itch.io,
OpenGameArt, Kenney) funcionam do mesmo jeito se o quadro for 16 × 14; confira a licença
(CC0 é o ideal; CC-BY pede crédito em `THIRD_PARTY_NOTICES.md`).

### Pôr a mascote em uso

1. Acrescente a entrada em `SPRITES` (`src/mascot/sprites.rs`) com `key` e `label`.
2. Ela já pode ser escolhida em todo lugar: o seletor de mascote mostra todas. Se ela
   combina com um app, ponha-a nas opções `mascot`/`*_mascot` do manifesto dele: essas são
   as sugeridas e aparecem primeiro na grade (o hub completa a lista com as outras).
3. `cargo test`: `todas_as_mascotes_tem_16x14` e `mascotes_pixel_tem_cor_para_cada_letra`
   pegam linha torta e letra sem cor.

## 7. Testar na tela

Rode uma cópia isolada, que não toca a sua config nem conversa com a instância aberta:

```sh
mkdir -p /tmp/mu-teste/miniutils
cp ~/.config/miniutils/config.toml /tmp/mu-teste/miniutils/
# troque `skin = "…"` ou o tema de um app no arquivo copiado
XDG_CONFIG_HOME=/tmp/mu-teste MINIUTILS_INSTANCE=teste cargo run
```

`--tab apps` abre direto nas configurações. Confira cada visual em painel, modo de edição
e configurações, e cada tema em calma e alarme.

Para ajustar a tela sem recompilar a cada mudança, rode com o live-preview do Slint: os
`.slint` recarregam ao salvar (num editor que grava no mesmo arquivo, como o VS Code).

```sh
cargo run --features live-preview
```

Um agente faz o mesmo, e ainda vê e clica, com `tools/dev-ui.sh` (ver AGENTS.md).

## 8. Limites do renderizador

O MiniUtils usa o renderizador **por software** do Slint (economiza ~240 MB de memória).
O que isso impõe:

| Limite | Como contornar |
|---|---|
| Sem blur, sem sombra (`drop-shadow`) | anéis concêntricos (aura da mascote), sombra dura com um retângulo deslocado (`HeaderButton` "chunky") |
| Gradiente com 3+ cores só desenha o primeiro trecho | vários retângulos, cada um com gradiente de 2 cores |
| `Path` não gira (`rotation-angle`) | um segundo ícone (`chevron` / `chevron-right`) |
| Padrões repetidos e gradientes radiais são caros | desenhe em Rust numa imagem (`backdrop.rs`) |
| Trocar uma imagem grande repinta tudo o que está por cima dela | animação do fundo com elementos pequenos sobre a imagem fixa (`ui/ambient.slint`), nunca uma imagem nova por quadro |
| `opacity` pinta o elemento numa camada à parte | cor com alfa (`cor.with-alpha(a)`) no `background` |
| `PopupWindow` sem altura explícita tem área clicável menor que o desenho | sempre dê `height` ao popup (`Select`) |
| `PopupWindow` é desenhado dentro da janela e não rola sozinho | lista longa precisa de altura máxima e `Flickable` (o `Select` mostra 8 opções e meia e rola o resto) |
| Recriar um modelo destrói os componentes dentro dele (um popup aberto fecha) | só refaça listas quando o conteúdo mudar (`rebuild_settings`) |
| Largura que depende da largura da janela dentro de um layout gera "binding loop" | tamanhos fixos ou `horizontal-stretch`, nunca `root.width` dentro do próprio layout |
| Cada imagem nova redesenha a região | o cache de quadros (`render::Cache`) só troca a imagem quando os pixels mudam |
