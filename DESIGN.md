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

Um visual é uma entrada na tabela `SKINS` (`src/skin/mod.rs`). Nada de layout muda.

### Criar um visual

1. Copie a entrada mais parecida em `SKINS` e troque `key` (sem espaços, vai para a config),
   `label` (aparece no seletor) e `app_theme` (tema dos apps em "auto"; precisa existir em
   `themes.rs`, um teste confere).
2. Preencha os campos (tabela abaixo). As cores do modo de edição (`edit-*`) e a borda do
   botão de perigo são derivadas do acento por `apply`; não precisa defini-las.
3. Escolha um **fundo** (`backdrop`). Para um fundo novo, veja abaixo.
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

Os 4 visuais atuais servem de referência:

| Visual | Ideia | Traços |
|---|---|---|
| Neon | cyberpunk | cantos cortados, grade de pontos, Chakra Petch, botões com ícone |
| Terminal | fósforo verde | tudo mono, prompt com cursor, `[ fixar ]`, barra de status, linhas de varredura |
| Aconchego | casinha de madeira | tema **claro**, borda de 6 px, papel pontilhado, Pixelify, botões "chunky" com sombra |
| Noite chuvosa | cidade à noite | lua, prédios, chuva, saudação com hora, cartões translúcidos |

### Fundos

Os fundos são desenhados em Rust (`src/skin/backdrop.rs`) numa imagem do tamanho da área do
painel, refeita quando ela muda. O `Canvas` tem `rect`, `glow` (brilho elíptico), `dots`
(grade de pontos) e `stripes` (linhas repetidas num ângulo), todos em **pixels lógicos**
(o `scale` converte).

Para um fundo novo: acrescente um caso em `enum Backdrop`, desenhe-o em `render` e use-o no
visual. Regras:

- Desenhe por cima de `bg`; o fundo não pode brigar com o texto dos apps (alfa baixo: 0,05–0,15).
- Elementos presos a um canto (lua, prédios) devem funcionar de 400 px a 2000 px de largura.
- Animação não: o fundo é uma imagem estática (redesenhar a cada quadro custaria CPU).
- O teste `todos_desenham_algo_alem_do_fundo` pega um fundo que saiu liso por engano.

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

Para que o tema apareça, acrescente a `key` nas `options` do `theme` de cada manifesto.

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

Regras gerais de interface:

- Alvos de clique com ≥ 36 px de altura (44 px quando for o botão principal).
- Use os componentes de `ui/widgets.slint` (`HeaderButton`, `TextButton`, `IconButton`,
  `SqSwitch`, `Stepper`, `Select`) em vez de desenhar botões novos. Eles já seguem o visual
  e têm papel de acessibilidade (`accessible-role`).
- Ícones: traço SVG num quadro 24×24 (`Icons.*`, desenhados com `Icon`); para o estilo
  "chunky", a versão pixel 7×7 (`Icons.px-*`). Ícone sem texto precisa de `label`.

## 5. Mini-apps

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
4. **Registro** — em `src/hub.rs`, `registry.add(crate::apps::<app>::<App>Mini::create(…))`.
5. **No painel** — em `ui/hub.slint`, importe a view e o global, exporte o global no
   `export { … }` e acrescente no `Dashboard` um `if c.id == "cyber.<nome>": <App>View { … }`.
6. **Provedor** (se precisar de dado novo) — `src/providers/<nome>.rs` com um `collect()`
   que devolve um snapshot; um caso novo em `bus::Event`; `bus.spawn("<nome>", intervalo, …)`
   no `Hub::new`. O provedor só roda enquanto algum app ativo o tiver em `uses`.
7. **Conversa entre mascotes** (opcional) — implemente `voices` (as mascotes na tela),
   `take_facts` (o que aconteceu: use `chatter::mood_fact` quando o humor muda e registre
   `Poked` ao cutucar) e `lines` (`include_str!` do `miniapps/<app>/lines.toml`, só com o
   que o app tem de próprio; o resto vem das genéricas). Na view, dê ao `Mascot` o
   `voice: "cyber.<nome>:<nome da mascote>"`, o mesmo `app:nome` da `Voice` no Rust.
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
- Flutuar, pular e tremer são do Slint (`Mascot`); piscar, LED e glitch são do Rust. Não
  desenhe quadros de animação no sprite.

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

## 8. Limites do renderizador

O MiniUtils usa o renderizador **por software** do Slint (economiza ~240 MB de memória).
O que isso impõe:

| Limite | Como contornar |
|---|---|
| Sem blur, sem sombra (`drop-shadow`) | anéis concêntricos (aura da mascote), sombra dura com um retângulo deslocado (`HeaderButton` "chunky") |
| Gradiente com 3+ cores só desenha o primeiro trecho | vários retângulos, cada um com gradiente de 2 cores |
| `Path` não gira (`rotation-angle`) | um segundo ícone (`chevron` / `chevron-right`) |
| Padrões repetidos e gradientes radiais são caros | desenhe em Rust numa imagem (`backdrop.rs`) |
| `PopupWindow` sem altura explícita tem área clicável menor que o desenho | sempre dê `height` ao popup (`Select`) |
| `PopupWindow` é desenhado dentro da janela e não rola sozinho | lista longa precisa de altura máxima e `Flickable` (o `Select` mostra 8 opções e meia e rola o resto) |
| Recriar um modelo destrói os componentes dentro dele (um popup aberto fecha) | só refaça listas quando o conteúdo mudar (`rebuild_settings`) |
| Largura que depende da largura da janela dentro de um layout gera "binding loop" | tamanhos fixos ou `horizontal-stretch`, nunca `root.width` dentro do próprio layout |
| Cada imagem nova redesenha a região | o cache de quadros (`render::Cache`) só troca a imagem quando os pixels mudam |
