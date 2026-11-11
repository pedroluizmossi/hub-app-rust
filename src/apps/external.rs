//! Apps externos (`[app]` no manifesto): um executável próprio, com acesso total, que o
//! host inicia ao ligar o app e encerra ao desligar. Ver o formato do manifesto no topo de
//! `core/plugins.rs`; a tela, o humor e a conversa são os mesmos dos plugins com tela
//! (`apps/plugin.rs`).
//!
//! ## Protocolo (versão 1)
//!
//! JSON, **uma mensagem por linha** (UTF-8, terminada em `\n`), sempre com o campo `type`.
//! O app lê da entrada padrão e escreve na saída padrão; a saída de erro vai para o
//! registro do hub (`app <id>: …`), então é lá que vai o que for depuração.
//!
//! Do host para o app (entrada padrão):
//! ```text
//! {"type":"start","api":1,"id":"io.x.app","os":"linux","data_dir":"…","settings":{…}}
//!                                  a primeira mensagem, sempre
//! {"type":"tick"}                  a cada [app] every segundos
//! {"type":"action","name":"…","arg":"…"}   a tela chamou `callback action(string, string)`
//! {"type":"event","topic":"sys","data":{…}}   leitura de um provedor de `uses`
//! {"type":"settings","settings":{…}}          o usuário mudou uma opção do app
//! {"type":"shown"}                 a janela do hub abriu (bom momento para dado novo)
//! {"type":"stop"}                  vai desligar: saia logo (depois de 2 s o host encerra)
//! ```
//! A entrada padrão fechar (EOF) também quer dizer "saia": é o que acontece se o hub cair.
//!
//! Do app para o host (saída padrão):
//! ```text
//! {"type":"set","prop":"total","value":42}    muda uma propriedade da tela (texto,
//!                                  número, bool, lista ou objeto, como no script)
//! {"type":"image","prop":"grafico","path":"/caminho/absoluto.png"}   imagem do disco
//! {"type":"mood","mood":"happy"}   sleep, idle, happy, busy, alarm
//! {"type":"summary","text":"…"}    linha do tooltip da bandeja
//! {"type":"notify","title":"…","body":"…"}
//! {"type":"fact","kind":"strain","subject":"CPU","value":93}   conversa entre mascotes
//! {"type":"error","message":"…"}   erro na célula e mascote em alarme ("" limpa)
//! ```
//! Linha que não é JSON do protocolo vai para o registro e é ignorada.
//!
//! O processo roda na pasta do app, com as variáveis `MINIUTILS_APP_ID`,
//! `MINIUTILS_APP_DATA` (pasta para os dados do app, que sobrevive a atualizações e sai ao
//! remover) e `MINIUTILS_APP_API` (versão do protocolo). Se ele sair sozinho, o app vai
//! para alarme com o código de saída; o botão de atualizar do cartão o inicia de novo.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::core::exec;

/// Versão do protocolo que este hub fala.
pub const PROTOCOL: u32 = 1;
/// Quanto o app tem para sair depois do `stop`.
const STOP_GRACE: Duration = Duration::from_secs(2);
/// Linha mais longa aceita do app.
const MAX_LINE: usize = 4 << 20;

/// Do host para o app.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToApp {
    Start { api: u32, id: String, os: &'static str, data_dir: String, settings: serde_json::Value },
    Tick,
    Action { name: String, arg: String },
    Event { topic: String, data: serde_json::Value },
    Settings { settings: serde_json::Value },
    Shown,
    Stop,
}

impl ToApp {
    /// A linha que vai para a entrada do app (com o `\n`).
    pub fn line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

/// Do app para o host.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromApp {
    Set {
        prop: String,
        value: serde_json::Value,
    },
    Image {
        prop: String,
        path: String,
    },
    Mood {
        mood: String,
    },
    Summary {
        text: String,
    },
    Notify {
        title: String,
        #[serde(default)]
        body: String,
    },
    Fact {
        kind: String,
        #[serde(default)]
        subject: String,
        #[serde(default)]
        value: Option<f64>,
    },
    Error {
        message: String,
    },
}

/// Uma linha da saída do app: a mensagem, ou por que não é uma.
pub fn parse_line(line: &str) -> Result<FromApp, String> {
    serde_json::from_str(line.trim()).map_err(|e| e.to_string())
}

/// O que chega do processo.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Msg(FromApp),
    /// Saiu sem o host pedir (a mensagem já vem pronta para a tela).
    Exited(String),
}

/// Um app externo rodando.
pub struct Process {
    /// Linhas para a thread que escreve na entrada do app (`None` = já parou).
    tx: Option<Sender<String>>,
    child: Arc<Mutex<Option<Child>>>,
    /// O host pediu para parar: a saída do processo não é falha.
    stopping: Arc<AtomicBool>,
}

/// Como iniciar o processo.
pub struct Launch<'a> {
    pub id: &'a str,
    pub program: &'a Path,
    /// Pasta em que roda (a do app).
    pub dir: &'a Path,
    pub data_dir: &'a Path,
}

impl Process {
    /// Inicia o app. `on` recebe cada mensagem (numa thread de leitura, não na da UI).
    pub fn spawn(l: &Launch, on: impl Fn(Incoming) + Send + 'static) -> Result<Process, String> {
        if !l.program.is_file() {
            return Err(format!("o executável {} não existe", l.program.display()));
        }
        let _ = std::fs::create_dir_all(l.data_dir);
        let mut cmd = exec::command_path(l.program);
        cmd.current_dir(l.dir)
            .env("MINIUTILS_APP_ID", l.id)
            .env("MINIUTILS_APP_DATA", l.data_dir)
            .env("MINIUTILS_APP_API", PROTOCOL.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // grupo próprio: ao encerrar, os filhos que o app abriu saem junto
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd.spawn().map_err(|e| format!("não consegui iniciar {}: {e}", l.program.display()))?;
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let child = Arc::new(Mutex::new(Some(child)));
        let stopping = Arc::new(AtomicBool::new(false));
        let name = |what: &str| format!("app-{what}-{}", l.id);

        // escrita: o host nunca espera um pipe cheio na thread da UI
        let (tx, rx) = channel::<String>();
        if let Some(mut stdin) = stdin {
            let _ = std::thread::Builder::new().name(name("in")).spawn(move || {
                for line in rx {
                    if stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).is_err() {
                        break;
                    }
                }
                // fim do canal: a entrada fecha e o app recebe EOF
            });
        }
        // saída de erro: para o registro
        if let Some(stderr) = stderr {
            let id = l.id.to_string();
            let _ = std::thread::Builder::new().name(name("err")).spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let line: String = line.chars().take(1000).collect();
                    log::info!("app {id}: {line}");
                }
            });
        }
        // saída: as mensagens
        if let Some(stdout) = stdout {
            let (id, child, stopping) = (l.id.to_string(), child.clone(), stopping.clone());
            let _ = std::thread::Builder::new().name(name("out")).spawn(move || {
                let end = read_messages(stdout, &id, &on);
                if !stopping.load(Ordering::SeqCst) {
                    let status = finish(&child, STOP_GRACE);
                    on(Incoming::Exited(match end {
                        Some(e) => e,
                        None => format!("o app saiu sozinho ({status})"),
                    }));
                }
            });
        }
        Ok(Process { tx: Some(tx), child, stopping })
    }

    pub fn send(&self, msg: &ToApp) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(msg.line());
        }
    }

    /// Pede para sair (`stop` e EOF) e, se não sair no prazo, encerra o grupo. Não espera.
    pub fn stop(&mut self) {
        let Some(tx) = self.tx.take() else { return };
        self.stopping.store(true, Ordering::SeqCst);
        let _ = tx.send(ToApp::Stop.line());
        drop(tx);
        let child = self.child.clone();
        let _ = std::thread::Builder::new().name("app-stop".into()).spawn(move || {
            finish(&child, STOP_GRACE);
        });
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Lê as linhas até o fim. `Some(erro)` se parou por um problema do app (linha grande demais).
fn read_messages(stdout: impl Read, id: &str, on: &impl Fn(Incoming)) -> Option<String> {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.by_ref().take(MAX_LINE as u64).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) if n >= MAX_LINE && buf.last() != Some(&b'\n') => {
                return Some(format!("o app mandou uma linha com mais de {} MiB", MAX_LINE >> 20));
            }
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(&line) {
            Ok(msg) => on(Incoming::Msg(msg)),
            Err(e) => {
                let shown: String = line.trim().chars().take(200).collect();
                log::warn!("app {id}: linha ignorada ({e}): {shown}");
            }
        }
    }
}

/// Espera o processo sair por até `grace`; depois encerra o grupo. Devolve o código de saída.
fn finish(child: &Mutex<Option<Child>>, grace: Duration) -> String {
    let Some(mut child) = child.lock().ok().and_then(|mut c| c.take()) else { return "já encerrado".into() };
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.to_string(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                exec::kill_group(&mut child);
                return "encerrado pelo host".into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mensagens_do_protocolo() {
        assert_eq!(ToApp::Tick.line(), "{\"type\":\"tick\"}\n");
        let a = ToApp::Action { name: "sort".into(), arg: "mem".into() }.line();
        assert_eq!(a, "{\"type\":\"action\",\"name\":\"sort\",\"arg\":\"mem\"}\n");
        assert_eq!(
            parse_line(r#"{"type":"set","prop":"n","value":[1,"a"]}"#).unwrap(),
            FromApp::Set { prop: "n".into(), value: serde_json::json!([1, "a"]) }
        );
        assert_eq!(
            parse_line(r#"{"type":"fact","kind":"strain","subject":"CPU","value":93}"#).unwrap(),
            FromApp::Fact { kind: "strain".into(), subject: "CPU".into(), value: Some(93.0) }
        );
        assert_eq!(parse_line(r#"{"type":"notify","title":"oi"}"#).unwrap(), FromApp::Notify { title: "oi".into(), body: String::new() });
        assert!(parse_line("print de depuração").is_err());
        assert!(parse_line(r#"{"type":"apagar_tudo"}"#).is_err());
    }

    #[test]
    fn linhas_ruins_sao_ignoradas_e_linha_enorme_para() {
        let got = Mutex::new(Vec::new());
        let on = |i: Incoming| got.lock().unwrap().push(i);
        let text = "lixo\n\n{\"type\":\"mood\",\"mood\":\"happy\"}\n{\"type\":\"summary\",\"text\":\"ok\"}";
        assert_eq!(read_messages(text.as_bytes(), "t", &on), None);
        assert_eq!(
            *got.lock().unwrap(),
            [Incoming::Msg(FromApp::Mood { mood: "happy".into() }), Incoming::Msg(FromApp::Summary { text: "ok".into() })]
        );
        let enorme = vec![b'x'; MAX_LINE + 10];
        assert!(read_messages(enorme.as_slice(), "t", &on).unwrap().contains("MiB"));
    }

    /// Um "app" de verdade (um script de shell): recebe o start, responde e sai no stop.
    #[cfg(unix)]
    #[test]
    fn processo_conversa_e_para() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("miniutils-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let prog = dir.join("app.sh");
        let script = r#"#!/bin/sh
echo "iniciando" >&2
while IFS= read -r line; do
  case "$line" in
    *'"start"'*) printf '{"type":"set","prop":"id","value":"%s"}\n' "$MINIUTILS_APP_ID" ;;
    *'"tick"'*) echo '{"type":"mood","mood":"busy"}' ;;
    *'"stop"'*) exit 0 ;;
  esac
done
"#;
        std::fs::write(&prog, script).unwrap();
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (tx, rx) = channel();
        let data = dir.join("dados");
        let launch = Launch { id: "io.x.app", program: &prog, dir: &dir, data_dir: &data };
        let mut p = Process::spawn(&launch, move |i| {
            let _ = tx.send(i);
        })
        .unwrap();
        assert!(data.is_dir(), "a pasta de dados é criada");
        p.send(&ToApp::Start { api: PROTOCOL, id: "io.x.app".into(), os: "linux", data_dir: String::new(), settings: serde_json::json!({}) });
        p.send(&ToApp::Tick);
        let t = Duration::from_secs(5);
        assert_eq!(rx.recv_timeout(t).unwrap(), Incoming::Msg(FromApp::Set { prop: "id".into(), value: "io.x.app".into() }));
        assert_eq!(rx.recv_timeout(t).unwrap(), Incoming::Msg(FromApp::Mood { mood: "busy".into() }));
        p.stop();
        // parou a pedido: nada de "saiu sozinho"
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());

        // um app que sai sozinho vira aviso
        std::fs::write(&prog, "#!/bin/sh\nexit 3\n").unwrap();
        let (tx, rx) = channel();
        let _p = Process::spawn(&launch, move |i| {
            let _ = tx.send(i);
        })
        .unwrap();
        match rx.recv_timeout(t).unwrap() {
            Incoming::Exited(msg) => assert!(msg.contains('3'), "{msg}"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
