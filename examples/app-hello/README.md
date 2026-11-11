# Olá, app externo (exemplo do MiniUtils)

Exemplo de **app externo**: a lógica é um executável próprio (`src/main.rs`, em Rust), que o
hub inicia ao ligar o app e com quem conversa por JSON, uma mensagem por linha, na entrada e
na saída padrão. A tela é o `view.slint`, como num plugin com tela.

- **Acesso total:** roda com as permissões do seu usuário. Este exemplo lê a pasta pessoal e
  grava o contador em `estado.json`, na pasta de dados que o hub dá (`MINIUTILS_APP_DATA`).
- Lê o provedor `sys` (CPU) e fica ocupado acima de 80%.
- Formato do manifesto: topo de `src/core/plugins.rs`. Protocolo: topo de
  `src/apps/external.rs`.

## Desenvolver

```sh
cargo build -p app-hello
mkdir -p examples/app-hello/bin
ln -sf ../../../target/debug/app-hello examples/app-hello/bin/app-hello
cargo run -- --plugin-dev examples/app-hello
```

No modo dev, o link em `bin/` vale, e cada `cargo build -p app-hello` reinicia o app no
hub. O que o app escreve com `eprintln!` aparece no terminal do hub. No Windows, copie
`target\debug\app-hello.exe` para `bin\app-hello.exe` (criar link pede privilégio).

## Publicar na loja

O executável não vai no repositório: o índice da loja aponta, por sistema, o endereço do
binário e o SHA-256 dele, e anuncia `"full_access": true`:

```json
{
  "id": "io.github.pedroluizmossi.app-hello",
  "full_access": true,
  "uses": ["sys"],
  "binaries": {
    "linux": { "url": "https://…/app-hello", "sha256": "…" },
    "windows": { "url": "https://…/app-hello.exe", "sha256": "…" }
  }
}
```

A instalação baixa o binário deste sistema, confere o hash e mostra o aviso de acesso total
antes de instalar.
