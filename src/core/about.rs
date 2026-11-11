//! Versão do app, para o `--version`, o registro e o rodapé das Configurações.

/// A `version` do Cargo.toml (a tag de lançamento é `v` + ela).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// "0.1.0 (Linux x86_64)".
pub fn version_line() -> String {
    format!("{VERSION} ({} {})", os_name(), std::env::consts::ARCH)
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn linha_da_versao() {
        let l = super::version_line();
        assert!(l.starts_with(env!("CARGO_PKG_VERSION")));
        assert!(l.contains(std::env::consts::ARCH));
    }
}
