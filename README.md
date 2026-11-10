# CyberHub

Hub de mini-apps para o desktop Linux, com mascotes em pixel art que reagem ao que está
acontecendo. Fica na bandeja do sistema e hospeda apps que você liga e desliga à vontade.
É a reescrita do plasmoid [CyberDock](../anime-cyber-docker) em **Rust + Slint**, sem depender do KDE.

Mini-apps incluídos:

- **Docker**: containers, stacks e ações rápidas (iniciar, parar, reiniciar, logs, shell). A
  mascote dorme sem containers, fica agitada sob carga e entra em alarme quando algo cai,
  entra em loop ou fica unhealthy. Também manda notificações.
- **Sinais vitais**: CPU, RAM, GPU (NVIDIA ou AMD) e disco, cada um com a própria mascote,
  histórico do último minuto e temperatura.

A arquitetura e o roteiro estão em [ARCHITECTURE.md](ARCHITECTURE.md).

## Compilar e rodar

Precisa de Rust (via [rustup](https://rustup.rs)).

```bash
cargo build --release
./target/release/cyberhub
```

Opções:

```text
--hidden     inicia só na bandeja (para o autostart)
--tab <id>   abre numa aba: cyber.docker, cyber.vitals ou apps
```

Clique na mascote da bandeja: o hub abre como popup junto do ícone e some quando você
clica fora ou aperta Esc.

A configuração fica em `~/.config/cyberhub/config.toml` e é editada pela aba **Apps**.

Variáveis de ambiente:

```text
CYBERHUB_NATIVE_WAYLAND=1   usa Wayland nativo em vez de XWayland (o popup perde a posição)
CYBERHUB_DEBUG=1            mostra eventos de foco e de janela no terminal
```

## Requisitos em tempo de execução

- Uma bandeja do sistema (StatusNotifierItem): KDE, XFCE, Cinnamon e MATE já têm. No GNOME,
  instale a extensão *AppIndicator*. Sem bandeja, o hub continua funcionando como janela.
- Docker acessível sem `sudo` (usuário no grupo `docker`).
- `nvidia-smi` para GPUs NVIDIA. Placas AMD são lidas direto do driver `amdgpu`.
- Um terminal para os botões de logs e shell: usa `$TERMINAL`, ou detecta konsole,
  gnome-terminal, kgx, xfce4-terminal, alacritty, kitty, foot e outros.

## Testes

```bash
cargo test
```

## Licença

MIT. A interface usa o [Slint](https://slint.dev) sob a Royalty-free Desktop License.
