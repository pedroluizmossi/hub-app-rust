//! Instância única: a primeira `cyberhub` escuta num socket Unix; as seguintes só mandam
//! um comando (`show`, `show apps`) para ela e saem.

use std::fs;
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Pedido de outra instância para a que já está rodando.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// Mostra o popup; com aba (`show apps`), abre nela.
    Show(Option<String>),
}

/// Lê uma linha do socket. Comando vazio ou desconhecido vira `None`.
pub fn parse(line: &str) -> Option<Command> {
    let mut words = line.split_whitespace();
    match words.next()? {
        "show" => Some(Command::Show(words.next().map(str::to_string))),
        _ => None,
    }
}

/// A linha que a segunda instância manda para a primeira.
pub fn show_message(tab: Option<&str>) -> String {
    match tab {
        Some(t) => format!("show {t}"),
        None => "show".to_string(),
    }
}

/// `$XDG_RUNTIME_DIR/cyberhub.sock` (ou `cyberhub-<nome>.sock` com `CYBERHUB_INSTANCE`).
pub fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let instance = std::env::var("CYBERHUB_INSTANCE").ok();
    // SAFETY: getuid não tem efeitos colaterais nem falha
    let uid = unsafe { libc::getuid() };
    socket_path_for(runtime.as_deref().map(Path::new), uid, instance.as_deref())
}

fn socket_path_for(runtime_dir: Option<&Path>, uid: u32, instance: Option<&str>) -> PathBuf {
    let stem = match instance.filter(|n| !n.is_empty()) {
        Some(name) => {
            // o nome vai para o caminho: só letras, números, '-' (sem '/')
            let clean: String =
                name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
            format!("cyberhub-{clean}")
        }
        None => "cyberhub".to_string(),
    };
    match runtime_dir.filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => dir.join(format!("{stem}.sock")),
        None => PathBuf::from(format!("/tmp/{stem}-{uid}.sock")),
    }
}

/// Resultado de tentar ser a instância principal.
pub enum Role {
    /// Já há uma instância: ela recebeu o comando; esta deve sair.
    Second,
    /// Esta é a principal: o socket está aberto (ver `Server::serve`).
    First(Server),
}

/// Socket da instância principal. Apaga o arquivo ao sair.
pub struct Server {
    path: PathBuf,
    listener: UnixListener,
}

impl Server {
    /// Atende os pedidos numa thread própria; `on_command` roda nessa thread.
    pub fn serve<F>(&self, on_command: F) -> io::Result<()>
    where
        F: Fn(Command) + Send + 'static,
    {
        let listener = self.listener.try_clone()?;
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                handle(stream, &on_command);
            }
        });
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn handle<F: Fn(Command)>(stream: UnixStream, on_command: &F) {
    // cliente que não manda nada não pode travar os outros
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut line = String::new();
    // limite de 256 bytes: uma linha de comando nunca passa disso
    if BufReader::new((&stream).take(256)).read_line(&mut line).is_ok() {
        if let Some(cmd) = parse(&line) {
            on_command(cmd);
        }
    }
}

/// Tenta ser a instância principal em `path`. Se já houver uma, entrega `message` a ela.
pub fn claim(path: &Path, message: &str) -> io::Result<Role> {
    // duas tentativas: a 2ª cobre o caso de outra instância ter subido bem na hora
    for _ in 0..2 {
        if let Ok(mut stream) = UnixStream::connect(path) {
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            let _ = stream.write_all(format!("{message}\n").as_bytes());
            return Ok(Role::Second);
        }
        // ninguém atende: socket velho de um processo morto (ou arquivo que sobrou)
        let _ = fs::remove_file(path);
        match UnixListener::bind(path) {
            Ok(listener) => return Ok(Role::First(Server { path: path.to_path_buf(), listener })),
            Err(e) if e.kind() == ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(ErrorKind::AddrInUse, "socket ocupado"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn test_path(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("cyberhub-test-{}-{name}.sock", std::process::id()));
        let _ = fs::remove_file(&p);
        p
    }

    #[test]
    fn parse_aceita_show_com_ou_sem_aba() {
        assert_eq!(parse("show\n"), Some(Command::Show(None)));
        assert_eq!(parse("  show apps \n"), Some(Command::Show(Some("apps".into()))));
        assert_eq!(parse("show cyber.vitals"), Some(Command::Show(Some("cyber.vitals".into()))));
    }

    #[test]
    fn parse_ignora_vazio_e_desconhecido() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("\n"), None);
        assert_eq!(parse("quit"), None);
        assert_eq!(parse("showx"), None);
    }

    #[test]
    fn mensagem_e_caminho_do_socket() {
        assert_eq!(show_message(None), "show");
        assert_eq!(show_message(Some("apps")), "show apps");
        let run = Path::new("/run/user/1000");
        assert_eq!(socket_path_for(Some(run), 1000, None), run.join("cyberhub.sock"));
        assert_eq!(socket_path_for(Some(run), 1000, Some("teste1")), run.join("cyberhub-teste1.sock"));
        assert_eq!(socket_path_for(Some(Path::new("")), 1000, None), PathBuf::from("/tmp/cyberhub-1000.sock"));
        assert_eq!(socket_path_for(None, 7, Some("a/b")), PathBuf::from("/tmp/cyberhub-a_b-7.sock"));
    }

    #[test]
    fn socket_velho_de_processo_morto_e_substituido() {
        // o arquivo fica depois que o listener morre, e ninguém mais atende
        let path = test_path("velho");
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        assert!(matches!(claim(&path, "show").unwrap(), Role::First(_)));
    }

    #[test]
    fn arquivo_comum_no_lugar_do_socket_e_substituido() {
        let path = test_path("arquivo");
        fs::write(&path, b"lixo").unwrap();
        assert!(matches!(claim(&path, "show").unwrap(), Role::First(_)));
    }

    #[test]
    fn segunda_instancia_entrega_o_comando_e_sai() {
        let path = test_path("segunda");
        let Role::First(server) = claim(&path, "show apps").unwrap() else {
            panic!("a primeira instância devia virar a principal");
        };
        let (tx, rx) = mpsc::channel();
        server.serve(move |cmd| {
            let _ = tx.send(cmd);
        }).unwrap();
        assert!(matches!(claim(&path, "show apps").unwrap(), Role::Second));
        let cmd = rx.recv_timeout(Duration::from_secs(2)).expect("comando não chegou");
        assert_eq!(cmd, Command::Show(Some("apps".into())));
    }

    #[test]
    fn socket_some_ao_sair_da_principal() {
        let path = test_path("saida");
        let Role::First(server) = claim(&path, "show").unwrap() else {
            panic!("a primeira instância devia virar a principal");
        };
        assert!(path.exists());
        drop(server);
        assert!(!path.exists());
    }
}
