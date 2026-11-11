//! Registro do que acontece (avisos, erros, pânicos) num arquivo e no terminal.
//!
//! O app fica na bandeja, sem terminal, e no Windows a versão final nem tem console: sem
//! arquivo, um erro some sem rastro. Cada linha vai para os dois lugares:
//! - o arquivo `miniutils.log` em `os::log_dir()` (`~/.local/state/miniutils/` no Linux,
//!   `%LOCALAPPDATA%\miniutils\logs\` no Windows); passando de `MAX_SIZE`, ele vira
//!   `miniutils.1.log` (a anterior é descartada) e um novo começa;
//! - a saída de erro, para quem roda do terminal (no Linux, o journal também pega).
//!
//! O código usa as macros do crate `log` (`log::warn!`, `log::info!`…). Do MiniUtils
//! entra a partir de `info` (`debug` com `MINIUTILS_DEBUG=1` ou `--plugin-dev`, que assim
//! mostra o `debug()` do script); das bibliotecas, só
//! avisos e erros. Um pânico em qualquer thread também é registrado, com o lugar e a pilha.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Tamanho a partir do qual o arquivo é trocado por um novo.
const MAX_SIZE: u64 = 1 << 20;
const FILE_NAME: &str = "miniutils.log";
const OLD_NAME: &str = "miniutils.1.log";

struct Logger {
    /// Nosso nível (`info` ou `debug`).
    level: LevelFilter,
    file: Mutex<Option<Sink>>,
}

struct Sink {
    dir: PathBuf,
    file: File,
    size: u64,
}

impl Sink {
    fn open(dir: &Path) -> Option<Sink> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(FILE_NAME);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() >= MAX_SIZE) {
            let _ = std::fs::rename(&path, dir.join(OLD_NAME));
        }
        let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Some(Sink { dir: dir.to_path_buf(), file, size })
    }

    fn write(mut self, line: &str) -> Option<Sink> {
        let _ = self.file.write_all(line.as_bytes());
        self.size += line.len() as u64;
        if self.size < MAX_SIZE {
            return Some(self);
        }
        // passou do limite: fecha (o Windows não renomeia arquivo aberto) e começa outro
        let dir = self.dir;
        drop(self.file);
        Sink::open(&dir)
    }
}

impl Log for Logger {
    fn enabled(&self, meta: &Metadata) -> bool {
        let ours = meta.target().starts_with(env!("CARGO_CRATE_NAME"));
        meta.level() <= if ours { self.level } else { LevelFilter::Warn }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_line(
            &chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            record.level(),
            record.target(),
            &record.args().to_string(),
        );
        eprint!("{line}");
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sink) = file.take() {
            *file = sink.write(&line);
        }
    }

    fn flush(&self) {
        if let Some(sink) = self.file.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            let _ = sink.file.flush();
        }
    }
}

/// "2026-11-11 14:03:22.120 WARN  core::config: mensagem\n" (sem o "miniutils::" do
/// começo do módulo, que é igual em todas).
fn format_line(time: &str, level: Level, target: &str, msg: &str) -> String {
    let prefix = concat!(env!("CARGO_CRATE_NAME"), "::");
    let target = target.strip_prefix(prefix).unwrap_or(target);
    format!("{time} {level:<5} {target}: {msg}\n")
}

/// Liga o registro e o aviso de pânico. Chamar uma vez, no começo do app (os comandos de
/// terminal, como a loja e o `--help`, não precisam). `debug` também registra os detalhes
/// (vale sozinho com `MINIUTILS_DEBUG=1`). Devolve onde fica o arquivo.
pub fn init(debug: bool) -> Option<PathBuf> {
    let debug = debug || std::env::var_os("MINIUTILS_DEBUG").is_some();
    let level = if debug { LevelFilter::Debug } else { LevelFilter::Info };
    let dir = super::os::log_dir();
    let sink = dir.as_deref().and_then(Sink::open);
    let path = sink.as_ref().map(|s| s.dir.join(FILE_NAME));
    let logger = Box::leak(Box::new(Logger { level, file: Mutex::new(sink) }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level.max(LevelFilter::Warn));
    }
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("sem nome");
        let trace = std::backtrace::Backtrace::force_capture();
        log::error!("pânico na thread \"{name}\": {info}\n{trace}");
        log::logger().flush();
    }));
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linha_sem_o_nome_do_crate() {
        let l = format_line("2026-11-11 10:00:00.000", Level::Warn, "miniutils::core::config", "ops");
        assert_eq!(l, "2026-11-11 10:00:00.000 WARN  core::config: ops\n");
        let l = format_line("t", Level::Error, "winit::window", "x");
        assert_eq!(l, "t ERROR winit::window: x\n");
    }

    #[test]
    fn troca_o_arquivo_quando_enche() {
        let dir = std::env::temp_dir().join(format!("miniutils-log-teste-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut sink = Sink::open(&dir);
        let line = "x".repeat(1000) + "\n";
        for _ in 0..(MAX_SIZE / 1000 + 5) {
            sink = sink.and_then(|s| s.write(&line));
        }
        assert!(dir.join(OLD_NAME).exists(), "o cheio virou o antigo");
        let now = std::fs::metadata(dir.join(FILE_NAME)).unwrap().len();
        assert!(now < MAX_SIZE, "o atual começou de novo");
        drop(sink);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
