//! Instância única: a primeira `miniutils` fica escutando; as seguintes só mandam um
//! comando (`show`, `show apps`) para ela e saem.
//!
//! O canal muda por sistema, atrás da mesma interface (`socket_path`, `claim`,
//! `Server::serve`, `notify_plugins_changed`): socket Unix no Linux (`linux.rs`), named
//! pipe no Windows (`windows.rs`).

#[cfg(unix)]
#[path = "instance/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "instance/windows.rs"]
mod imp;

pub use imp::{Server, claim, notify_plugins_changed, socket_path};

/// Pedido de outra instância para a que já está rodando.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// Mostra o popup; com aba (`show apps`), abre nela.
    Show(Option<String>),
    /// Os plugins instalados mudaram (a loja pela linha de comando): relê a pasta.
    ReloadPlugins,
}

/// Lê uma linha recebida. Comando vazio ou desconhecido vira `None`.
pub fn parse(line: &str) -> Option<Command> {
    let mut words = line.split_whitespace();
    match words.next()? {
        "show" => Some(Command::Show(words.next().map(str::to_string))),
        "plugins" => Some(Command::ReloadPlugins),
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

/// Nome base do canal: `miniutils`, ou `miniutils-<nome>` com `MINIUTILS_INSTANCE` (para
/// rodar duas instâncias separadas, ex.: uma de teste).
fn stem(instance: Option<&str>) -> String {
    match instance.filter(|n| !n.is_empty()) {
        // o nome vai para o caminho: só letras, números, '-' (sem '/')
        Some(name) => format!("miniutils-{}", clean(name)),
        None => "miniutils".to_string(),
    }
}

fn clean(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

/// Resultado de tentar ser a instância principal.
pub enum Role {
    /// Já há uma instância: ela recebeu o comando; esta deve sair.
    Second,
    /// Esta é a principal: o socket está aberto (ver `Server::serve`).
    First(Server),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_aceita_show_com_ou_sem_aba() {
        assert_eq!(parse("show\n"), Some(Command::Show(None)));
        assert_eq!(parse("  show apps \n"), Some(Command::Show(Some("apps".into()))));
        assert_eq!(parse("show cyber.vitals"), Some(Command::Show(Some("cyber.vitals".into()))));
        assert_eq!(parse("plugins\n"), Some(Command::ReloadPlugins));
    }

    #[test]
    fn mensagem_e_nome_do_canal() {
        assert_eq!(show_message(None), "show");
        assert_eq!(show_message(Some("apps")), "show apps");
        assert_eq!(stem(None), "miniutils");
        assert_eq!(stem(Some("")), "miniutils");
        assert_eq!(stem(Some("a/b")), "miniutils-a_b");
    }

    #[test]
    fn parse_ignora_vazio_e_desconhecido() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("\n"), None);
        assert_eq!(parse("quit"), None);
        assert_eq!(parse("showx"), None);
    }
}
