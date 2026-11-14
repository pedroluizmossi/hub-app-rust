//! App version, for `--version`, the log and the Settings footer.

/// The `version` of Cargo.toml (the release tag is `v` + it).
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
    fn version_line() {
        let l = super::version_line();
        assert!(l.starts_with(env!("CARGO_PKG_VERSION")));
        assert!(l.contains(std::env::consts::ARCH));
    }
}
