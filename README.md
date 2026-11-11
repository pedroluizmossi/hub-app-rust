# MiniUtils

Hub de mini-apps para o desktop (Linux e Windows), com mascotes em pixel art que reagem ao que está
acontecendo. Fica na bandeja do sistema e hospeda apps que você liga e desliga à vontade.
É a reescrita do plasmoid [CyberDock](../anime-cyber-docker) em **Rust + Slint**, sem depender do KDE.

Mini-apps incluídos:

- **Docker**: containers, stacks e ações rápidas (iniciar, parar, reiniciar, logs, shell). A
  mascote dorme sem containers, fica agitada sob carga e entra em alarme quando algo cai,
  entra em loop ou fica unhealthy. Também manda notificações.
- **Sinais vitais**: CPU, RAM, GPU (NVIDIA ou AMD) e disco, cada um com a própria mascote,
  histórico do último minuto e temperatura.
- **CD Player**: o que está tocando, pelo MPRIS: disco em pixel art girando, faixa, artista,
  progresso e controles de tocar/pausar, faixa e avançar/voltar. Sem player aberto, fica em
  "sem disco"; com a opção ligada, notifica quando a faixa muda.
- **Assinaturas de IA**: janelas de uso do Codex, Claude Code e OpenCode Go e créditos do
  Kiro. Ligue o app em **Configurações › Apps**; ele começa desligado.
- **Portas abertas**: o que está escutando em cada porta (pelo `ss`), com o programa, se é só
  local ou aberta para a rede, atalho para abrir no navegador e botão de encerrar o processo
  (dois cliques para confirmar). Sem root, só aparecem os processos do seu usuário. Começa
  desligado.
- **To-do rápido**: lista curta de tarefas, salva em `~/.config/miniutils/todo.json`. Enter
  acrescenta, clique marca como feita; a mascote comemora quando a lista zera. Começa
  desligado.
- **Agentes de IA**: as sessões do Claude Code, Codex e OpenCode: quem está trabalhando (e
  em quê: a última ferramenta usada, sem o comando inteiro), quem espera por você e quem
  terminou, com os subagentes contados na linha de quem os disparou. Cada sessão é um cartão com
  o que a ferramenta informa (no Claude: modelo, esforço, branch, tokens, duração do último
  turno, último pedido e os subagentes). Clique no cartão mostra as opções: abrir a pasta
  do projeto, abrir um terminal nela e, em sessão parada do Codex ou do OpenCode, retomar
  a sessão. Só lê o que as ferramentas já gravam no disco, sem mexer na configuração
  delas; pode notificar quando um agente termina ou espera. Começa desligado.

Em **Configurações › Apps › CD Player**, a opção **Mostrar música e controles no menu da
bandeja** acrescenta a faixa e os controles ao menu do ícone. A faixa também aparece no
tooltip da bandeja. No GNOME é necessária uma extensão de AppIndicator.

### Assinaturas de IA

O MiniUtils consulta diretamente os serviços usando os logins locais dos clientes oficiais.
Os endpoints de Claude, Codex e Kiro não são públicos e podem mudar. A variável
`KIRO_API_KEY` tem prioridade sobre o login OAuth no banco do Kiro. A variável deve
estar presente no ambiente em que o MiniUtils é iniciado.

As consultas ocorrem somente enquanto o mini-app está ligado: Claude, Codex e OpenCode a
cada três minutos, Kiro a cada quinze. **Atualizar** antecipa as leituras, respeitando a
pausa após HTTP 429. Em falhas de conexão, o último valor é marcado como antigo; falhas de
autenticação removem a leitura anterior. O cartão
do Kiro mostra créditos incluídos, sem somar créditos adicionais.

Quando o ritmo da última hora indica que um limite acaba antes de renovar, a linha da
janela mostra a previsão ("Acaba ~16:10 · renova 18:00"). Se o ritmo dos últimos vinte
minutos aponta o mesmo, a mascote fica agitada mesmo abaixo do percentual configurado e
avisa as outras. A
previsão precisa de quinze minutos de leituras, só vale para janelas com horário de
renovação e pode ser desligada nas opções do app. Ao lado do percentual, uma linha fina
mostra o uso da última hora. No modo compacto o texto e a linha não aparecem.

O Claude usa o login OAuth de `~/.claude/.credentials.json` (ou `CLAUDE_CONFIG_DIR`). Se o
token estiver vencendo, o MiniUtils tenta renová-lo e gravar o resultado no mesmo arquivo,
preservando os demais campos. Isso compartilha credenciais com o Claude Code: se ambos
renovarem simultaneamente, pode ser necessário entrar novamente no Claude. Não imprima nem
compartilhe esse arquivo. O uso direto não exige uma sessão ativa do Claude Code.

Para Codex, Kiro e OpenCode, o MiniUtils lê as credenciais existentes sem renová-las. Quando
expirarem, atualize o login no cliente correspondente; o cartão informará a falha. Nenhuma
conversa é lida ou salva.

A arquitetura e o roteiro estão em [ARCHITECTURE.md](ARCHITECTURE.md). Como criar visuais,
temas, telas, mini-apps e mascotes está em [DESIGN.md](DESIGN.md).

## Criar apps

Dá para estender o hub de quatro jeitos: **plugin simples** (a saída de um comando, só com
manifesto), **plugin com tela** (tela própria e um script isolado), **app externo** (um
executável seu, em qualquer linguagem, com acesso total) e **mini-app embutido** (Rust
dentro do hub). Os três primeiros se instalam pela
[loja](https://github.com/pedroluizmossi/miniutils-store), sem recompilar o hub. Qual
escolher, o que cada um pode e os exemplos de cada: [APPS.md](APPS.md).

## Instalar

Uma linha instala (ou atualiza) a última versão publicada em
[Releases](https://github.com/pedroluizmossi/hub-app-rust/releases), só para o seu usuário,
sem senha de administrador. O script confere a soma SHA-256 do pacote antes de instalar.

**Linux** (x86_64, glibc 2.35 ou mais nova: Ubuntu 22.04+, Fedora 36+, Debian 12+, Arch…):

```bash
curl -fsSL https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.sh | sh
```

Instala em `~/.local/bin/miniutils`, com atalho no menu de aplicativos.

**Windows 10 ou 11** (no PowerShell):

```powershell
irm https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.ps1 | iex
```

Instala em `%LOCALAPPDATA%\Programs\MiniUtils`, com atalho no Menu Iniciar. Se o MiniUtils
estiver aberto, ele é fechado para a troca e aberto de novo na bandeja. O executável não
é assinado: baixado pelo navegador, o Windows pode mostrar o aviso do SmartScreen ("Mais
informações › Executar assim mesmo").

Para desinstalar (a configuração é mantida):

```bash
# Linux
curl -fsSL https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.sh | sh -s -- --uninstall
```

```powershell
# Windows
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/pedroluizmossi/hub-app-rust/master/install.ps1))) -Uninstall
```

Também dá para baixar o `.tar.gz` ou o `.zip` da página do Release e rodar o `miniutils`
de dentro da pasta.

## Compilar e rodar

Precisa de Rust (via [rustup](https://rustup.rs)).

```bash
cargo build --release
./target/release/miniutils
```

Opções:

```text
--hidden     inicia só na bandeja (para o autostart)
--tab <nome> abre numa página do painel (pelo nome, ex.: Geral) ou em apps (configurações)
--version    mostra a versão (também em Configurações › Sistema)
```

Para abrir junto com o sistema, ligue **Configurações › Sistema › Iniciar com o sistema**
(só na bandeja). No Linux isso grava `~/.config/autostart/miniutils.desktop`; no Windows,
a entrada "MiniUtils" em Gerenciador de Tarefas › Inicializar.

### Registros (logs)

Avisos, erros e travamentos vão para um arquivo, além do terminal:

- Linux: `~/.local/state/miniutils/miniutils.log`
- Windows: `%LOCALAPPDATA%\miniutils\logs\miniutils.log`

O botão **Abrir registros** em Configurações › Sistema abre a pasta. Ao relatar um
problema, mande esse arquivo e a versão. Passando de 1 MiB, o arquivo vira
`miniutils.1.log` e um novo começa. `MINIUTILS_DEBUG=1` registra também os detalhes.

Clique na mascote da bandeja: o hub abre como popup junto do ícone e some quando você
clica fora ou aperta Esc.

A configuração fica em `~/.config/miniutils/config.toml` (na primeira execução, a de
`~/.config/cyberhub/` é copiada para lá). A linha `name = "MiniUtils"` muda o nome exibido
na janela, na bandeja e nas notificações.

A engrenagem abre as **Configurações**. O botão de layout (ícone de grade) liga o modo de
edição: arraste cada app pelo topo e puxe as bordas para mudar o tamanho (as setas também
movem; Shift + setas redimensiona).

O visual da janela (Neon, Terminal, Aconchego, Noite chuvosa) é escolhido em
**Configurações › Aparência**. Cada visual é uma tabela de valores em `src/skin/mod.rs`
(cores, fontes, moldura, cabeçalho, botões) com um fundo desenhado em `src/skin/backdrop.rs`;
apps com o tema "Acompanha a janela" mudam junto.

As fontes embutidas (Chakra Petch, JetBrains Mono e Pixelify Sans, em `ui/fonts/`) são distribuídas sob a
SIL Open Font License; as licenças estão na mesma pasta.

Variáveis de ambiente:

```text
MINIUTILS_NATIVE_WAYLAND=1   usa Wayland nativo em vez de XWayland (o popup perde a posição)
MINIUTILS_DEBUG=1            registra também os detalhes (foco, janela, debug() dos plugins)
```

## Requisitos em tempo de execução

### Linux

- Uma bandeja do sistema (StatusNotifierItem): KDE, XFCE, Cinnamon e MATE já têm. No GNOME,
  instale a extensão *AppIndicator*. Sem bandeja, o hub continua funcionando como janela.
- Docker acessível sem `sudo` (usuário no grupo `docker`).
- Um player MPRIS para o CD Player (Firefox, Chromium, VLC, mpv, Spotify…). Sem player, o
  app mostra "sem disco".
- `nvidia-smi` para GPUs NVIDIA. Placas AMD são lidas direto do driver `amdgpu`.
- Um terminal para os botões de logs e shell: usa `$TERMINAL`, ou detecta konsole,
  gnome-terminal, kgx, xfce4-terminal, alacritty, kitty, foot e outros.

### Windows

Windows 10 ou 11. O mesmo código, com o equivalente nativo de cada parte:

- Ícone na área de notificação (clique abre o popup; clique direito abre o menu).
- Docker Desktop para o app Docker (`docker` no PATH).
- CD Player pelos controles de mídia do sistema: o que aparece no painel de volume
  (Spotify, navegadores, players).
- `nvidia-smi` para GPUs NVIDIA. Placas AMD e Intel ainda não são lidas no Windows.
- Sinais vitais sem carga média e sem temperatura da CPU (o Windows não as entrega sem
  driver ou privilégio).
- Portas abertas pelo `netstat`; "encerrar" fecha o processo na hora (não há o pedido
  educado do Linux).
- Logs e shell abrem no Windows Terminal (`wt`); sem ele, no console clássico.

Compilar: `cargo build --release` num Windows com o Rust instalado gera
`target\release\miniutils.exe`.

## Testes

```bash
cargo test
```

## CI e lançamentos

- **CI** (`.github/workflows/ci.yml`): todo push em `master` e todo pull request roda
  `cargo check` e `cargo test` em Ubuntu e Windows.
- **Lançamento** (`.github/workflows/release.yml`): uma tag `vX.Y.Z` igual à `version` do
  `Cargo.toml` compila a versão final nos dois sistemas e publica um Release com
  `miniutils-<versão>-linux-x86_64.tar.gz`, `miniutils-<versão>-windows-x86_64.zip` e as
  somas SHA-256. Os testes não rodam nele: crie a tag num commit que passou no CI. O
  `install.sh` e o `install.ps1` dependem desses nomes de arquivo; mudou um, mude os dois.

  ```bash
  git tag v0.2.0 && git push origin v0.2.0
  ```

  Tag com hífen (`v0.2.0-rc1`) sai como pré-lançamento. Rodar o workflow à mão (aba
  Actions) só gera os pacotes como artefatos, sem publicar.

## Licença

MIT. A interface usa o [Slint](https://slint.dev) sob a Royalty-free Desktop License.
