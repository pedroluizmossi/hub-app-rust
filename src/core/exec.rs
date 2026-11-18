//! Command executor with a per-mini-app allowlist.
//!
//! A mini-app only runs the programs declared in `permissions.exec` of its manifest, and
//! the arguments go straight to the process, without a shell — no string interpolation.

use std::ffi::OsStr;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::tr;

/// Limit for an action the user asked for (`docker stop` waits for the container to exit;
/// `compose restart` on a big stack takes a while).
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(180);

/// Why a command returned no output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// The mini-app did not declare the program in `permissions.exec`.
    Denied(String),
    /// The program is not installed (not on the PATH).
    NotFound(String),
    /// Did not finish in time and was killed.
    Timeout { program: String, after: Duration },
    /// Exited with an error: the message is the stderr (or the exit code).
    Failed(String),
}

impl ExecError {
    pub fn not_found(&self) -> bool {
        matches!(self, ExecError::NotFound(_))
    }

    /// First line of the message, short enough for the screen.
    pub fn brief(&self) -> String {
        self.to_string().lines().next().unwrap_or_default().chars().take(160).collect()
    }
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecError::Denied(msg) | ExecError::Failed(msg) => f.write_str(msg),
            ExecError::NotFound(program) => f.write_str(&tr!("{} is not installed", program)),
            ExecError::Timeout { program, after } => {
                f.write_str(&tr!("{} did not respond within {} s", program, after.as_secs()))
            }
        }
    }
}

impl std::error::Error for ExecError {}

#[derive(Debug, Clone)]
pub struct Exec {
    app: String,
    allowed: Vec<String>,
}

impl Exec {
    pub fn new(app: &str, allowed: &[String]) -> Self {
        Exec { app: app.to_string(), allowed: allowed.to_vec() }
    }

    fn check(&self, program: &str) -> Result<(), ExecError> {
        if self.allowed.iter().any(|p| p == program) {
            Ok(())
        } else {
            Err(ExecError::Denied(tr!("the mini-app {} is not allowed to run \"{}\"", self.app, program)))
        }
    }

    /// Runs and waits (with a deadline) on the caller's thread: for callers that already
    /// have their own thread (a plugin's script). Never on the UI thread.
    pub fn run_blocking(&self, program: &str, args: &[&str], limit: Duration) -> Result<String, ExecError> {
        self.check(program)?;
        run_timeout(program, args, limit)
    }

    /// Runs in the background (own thread) and calls `done` with the stdout, or the error.
    /// The command is killed if it takes longer than `ACTION_TIMEOUT`.
    pub fn run_async<F>(&self, program: &str, args: &[&str], done: F)
    where
        F: FnOnce(Result<String, ExecError>) + Send + 'static,
    {
        if let Err(e) = self.check(program) {
            done(Err(e));
            return;
        }
        let program = program.to_string();
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        std::thread::spawn(move || done(run_timeout(&program, &args, ACTION_TIMEOUT)));
    }

    /// Opens a terminal running `argv` (the program must be on the allowlist).
    pub fn open_terminal(&self, title: &str, argv: &[&str]) -> anyhow::Result<()> {
        self.open_terminal_in(title, None, argv)
    }

    /// Like `open_terminal`, starting in folder `dir` (`None` = the home folder).
    pub fn open_terminal_in(&self, title: &str, dir: Option<&Path>, argv: &[&str]) -> anyhow::Result<()> {
        let program = argv.first().ok_or_else(|| anyhow::anyhow!("empty command"))?;
        self.check(program)?;
        // full path: the terminal may not have the user's folders on its PATH
        let path = locate(program).ok_or_else(|| ExecError::NotFound(program.to_string()))?;
        let dir = dir.filter(|d| d.is_dir());
        let mut cmd = terminal_command(title, dir, Some((&path, &argv[1..])))?;
        // with no folder asked for, start in the home folder, not where the app was launched
        match dir {
            Some(dir) => {
                cmd.current_dir(dir);
            }
            None => {
                if let Some(home) = dirs::home_dir() {
                    cmd.current_dir(home);
                }
            }
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        spawn_detached(&mut cmd)?;
        Ok(())
    }
}

/// What a command wrote, whether it succeeded or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    /// "exit status: 1", for the error message when the stderr is empty.
    status: String,
}

/// Runs a program and returns its stdout; past `limit`, kills it and returns `Timeout`.
/// For the host's own use (providers) and the base of `Exec::run_async`.
pub fn run_timeout<S: AsRef<OsStr>>(program: &str, args: &[S], limit: Duration) -> Result<String, ExecError> {
    let out = output_timeout(program, args, limit)?;
    if out.success {
        Ok(out.stdout)
    } else {
        let err = out.stderr.trim();
        Err(ExecError::Failed(if err.is_empty() { tr!("{} exited with {}", program, out.status) } else { err.to_string() }))
    }
}

/// Like `run_timeout`, but returns the output even when the program exits with an error
/// (`docker inspect` of several ids fails if one is gone, and the others come on stdout).
///
/// The child gets its own process group (on Windows, its tree): when the deadline passes
/// the whole group dies, and no grandchild keeps the pipes open (which would block the
/// readers).
pub fn output_timeout<S: AsRef<OsStr>>(program: &str, args: &[S], limit: Duration) -> Result<Output, ExecError> {
    let mut cmd = command(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ExecError::NotFound(program.to_string()),
            _ => ExecError::Failed(format!("{program}: {e}")),
        })?;

    // One reader per pipe (otherwise a full pipe blocks the child); each one signals when
    // its pipe closes, and we wait for that signal with a deadline, without busy waking.
    let (tx, rx) = mpsc::channel::<(bool, Option<Vec<u8>>)>();
    let mut readers = 0;
    let pipes: [(bool, Option<Box<dyn Read + Send>>); 2] = [
        (true, child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
        (false, child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
    ];
    for (is_stdout, pipe) in pipes {
        let Some(mut pipe) = pipe else { continue };
        let tx = tx.clone();
        readers += 1;
        std::thread::spawn(move || {
            let _ = tx.send((is_stdout, read_capped(&mut pipe, MAX_OUTPUT)));
        });
    }
    drop(tx);

    let deadline = Instant::now() + limit;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut timed_out = false;
    for _ in 0..readers {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((true, Some(buf))) => stdout = buf,
            Ok((false, Some(buf))) => stderr = buf,
            // past the cap: stop now instead of buffering a runaway output
            Ok((_, None)) => {
                kill_group(&mut child);
                return Err(ExecError::Failed(tr!("{} wrote more than {} MiB", program, MAX_OUTPUT >> 20)));
            }
            Err(_) => {
                timed_out = true;
                break;
            }
        }
    }
    // pipes closed: the child is exiting (or has exited); the rest of the time is for it to finish
    let status = loop {
        if timed_out {
            break None;
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => break None,
        }
    };
    let Some(status) = status else {
        kill_group(&mut child);
        return Err(ExecError::Timeout { program: program.to_string(), after: limit });
    };
    Ok(Output {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        status: status.to_string(),
    })
}

/// Most a command may write on each channel: past this, it is killed (a plugin's `run`
/// or a misbehaving tool would otherwise fill the memory within the deadline).
const MAX_OUTPUT: usize = 8 << 20;

/// Reads `pipe` to the end, or `None` when it has more than `max` bytes.
fn read_capped(pipe: &mut dyn Read, max: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let _ = pipe.take(max as u64 + 1).read_to_end(&mut buf);
    (buf.len() <= max).then_some(buf)
}

/// `Command` for a program that runs in the background (providers, app actions).
///
/// On Windows it does two things `Command::new` doesn't: it finds programs that are
/// `.cmd`/`.bat` (CLIs installed by npm: `codex`, `claude`, `opencode`), which only run by
/// their full path, and it does not open a console window on every run.
pub fn command(program: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = match find(program) {
            Some(path) => Command::new(path),
            None => Command::new(program),
        };
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(windows))]
    Command::new(program)
}

/// `CREATE_NO_WINDOW`: console program without a window (otherwise it flashes on screen).
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `Command` for an executable given by path (an external app's): no PATH lookup and, on
/// Windows, no console window.
pub fn command_path(path: &Path) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Kills the child and its group, and reaps the process.
pub(crate) fn kill_group(child: &mut std::process::Child) {
    // SAFETY: `kill` only sends a signal; the pid is the child's, not reaped yet, and it
    // leads its own group (`process_group(0)`).
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    // Windows has no group to signal: `taskkill /T` ends the child and its grandchildren
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Opens an http(s) address in the default browser (a host service, outside the allowlist).
pub fn open_url(url: &str) {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return;
    }
    shell_open(OsStr::new(url));
}

/// Opens a folder in the default file manager (a host service, outside the allowlist).
/// Only existing folders: no files or addresses are opened this way.
pub fn open_dir(dir: &Path) -> bool {
    dir.is_absolute() && dir.is_dir() && shell_open(dir.as_os_str())
}

/// Hands an address or folder to the system's default program.
#[cfg(not(windows))]
fn shell_open(target: &OsStr) -> bool {
    spawn_detached(Command::new("xdg-open").arg(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())).is_ok()
}

/// Hands an address or folder to the system's default program. Through `ShellExecute`,
/// not `cmd /c start`: so nothing in the address is interpreted as a command.
#[cfg(windows)]
fn shell_open(target: &OsStr) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, PCWSTR, w};
    let target = HSTRING::from(target);
    // SAFETY: the strings live until the call returns; the result is just a code (> 32 = opened).
    let code = unsafe { ShellExecuteW(None, w!("open"), &target, PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
    code.0 as isize > 32
}

/// Launches a program that goes on by itself (browser, terminal, file manager). A thread
/// waits for it to exit: without the `wait`, the process stays a zombie until the app
/// closes, and the app stays in the tray for days.
fn spawn_detached(cmd: &mut Command) -> std::io::Result<()> {
    let mut child = cmd.spawn()?;
    let _ = std::thread::Builder::new().name("miniutils-reap".into()).spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Opens a terminal in folder `dir`, with the user's shell (a host service, outside the
/// allowlist: it runs no program chosen by the mini-app).
pub fn open_terminal_at(dir: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(dir.is_absolute() && dir.is_dir(), "{}", tr!("folder not found: {}", dir.display()));
    let mut cmd = terminal_command("", Some(dir), None)?;
    cmd.current_dir(dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    spawn_detached(&mut cmd)?;
    Ok(())
}

/// Builds the command that opens a terminal: in `dir` (if given) and running `run` (the
/// program, with its full path, and the arguments) or, without `run`, the user's shell.
#[cfg(not(windows))]
fn terminal_command(title: &str, dir: Option<&Path>, run: Option<(&Path, &[&str])>) -> anyhow::Result<Command> {
    let (term, prefix) = terminal().ok_or_else(|| anyhow::anyhow!("{}", tr!("no terminal found (set $TERMINAL)")))?;
    let mut cmd = Command::new(&term);
    cmd.args(terminal_title_args(&term, title));
    // konsole may reuse a running process, in another folder: pass it the folder
    if let (Some(dir), true) = (dir, term.ends_with("konsole")) {
        cmd.arg("--workdir").arg(dir);
    }
    if let Some((path, args)) = run {
        cmd.args(prefix).arg(path).args(args);
    }
    Ok(cmd)
}

/// Builds the command that opens a terminal on Windows: Windows Terminal (`wt`) when
/// present; otherwise the classic console (`conhost`). Both get the program and the
/// arguments directly, without going through `cmd` — no string interpolation.
#[cfg(windows)]
fn terminal_command(title: &str, dir: Option<&Path>, run: Option<(&Path, &[&str])>) -> anyhow::Result<Command> {
    let (path, args): (PathBuf, Vec<String>) = match run {
        Some((path, args)) => (path.to_path_buf(), args.iter().map(|a| a.to_string()).collect()),
        None => (default_shell(), Vec::new()),
    };
    // a `.cmd`/`.bat` always runs through `cmd`, which would interpret these characters
    if is_batch(&path) && args.iter().any(|a| a.contains(CMD_SPECIAL)) {
        anyhow::bail!("{}", tr!("argument with a special cmd character: not opening {} like that", path.display()));
    }
    let home = dirs::home_dir();
    let dir = dir.or(home.as_deref());
    let mut cmd = match find("wt") {
        Some(wt) => {
            let mut cmd = Command::new(wt);
            cmd.arg("new-tab");
            if !title.is_empty() {
                cmd.args(["--title", title]);
            }
            if let Some(dir) = dir {
                cmd.arg("-d").arg(dir);
            }
            // for `wt`, `;` separates commands: escaped, it becomes text
            cmd.arg(&path).args(args.iter().map(|a| a.replace(';', "\\;")));
            cmd
        }
        None => {
            let mut cmd = Command::new("conhost.exe");
            cmd.arg(&path).args(&args);
            cmd
        }
    };
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    Ok(cmd)
}

/// Characters `cmd` interprets in a command line.
#[cfg(windows)]
const CMD_SPECIAL: &[char] = &['&', '|', '<', '>', '^', '%', '"', '!', '(', ')', '\n', '\r'];

#[cfg(windows)]
fn is_batch(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

/// The user's shell (`$SHELL`), or `/bin/sh`.
#[cfg(not(windows))]
pub fn default_shell() -> PathBuf {
    std::env::var_os("SHELL").map(PathBuf::from).filter(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

/// Shell for "open terminal here": the new PowerShell, the one bundled with Windows, or `cmd`.
#[cfg(windows)]
pub fn default_shell() -> PathBuf {
    find("pwsh")
        .or_else(|| find("powershell"))
        .or_else(|| std::env::var_os("COMSPEC").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("cmd.exe"))
}

pub fn which(program: &str) -> bool {
    find(program).is_some()
}

/// Path of a program on the PATH.
pub fn find(program: &str) -> Option<PathBuf> {
    find_in(path_dirs(), program)
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default()
}

fn find_in(dirs: Vec<PathBuf>, program: &str) -> Option<PathBuf> {
    let names = file_names(program);
    dirs.into_iter().find_map(|d| names.iter().map(|n| d.join(n)).find(|p| p.is_file()))
}

/// File names a program may have: on Linux, its own name.
#[cfg(not(windows))]
fn file_names(program: &str) -> Vec<String> {
    vec![program.to_string()]
}

/// File names a program may have: on Windows, the name with each executable extension of
/// `PATHEXT` (`docker` → `docker.exe`, `codex` → `codex.cmd`).
#[cfg(windows)]
fn file_names(program: &str) -> Vec<String> {
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    windows_file_names(program, &pathext)
}

/// Separate from the environment so it can be tested on any system.
#[cfg(any(windows, test))]
fn windows_file_names(program: &str, pathext: &str) -> Vec<String> {
    let mut names = Vec::new();
    // already has an extension (`wt.exe`): that name first
    if Path::new(program).extension().is_some() {
        names.push(program.to_string());
    }
    names.extend(pathext.split(';').filter(|e| e.starts_with('.') && e.len() > 1).map(|e| format!("{program}{}", e.to_ascii_lowercase())));
    names
}

/// Path of a program: on the PATH or in the folders where CLI installers usually put user
/// binaries. Launched from the system menu, the app may not have those folders on its PATH
/// (they come from `.bashrc`/`.zshrc`), and the terminal would not find the program.
fn locate(program: &str) -> Option<PathBuf> {
    let mut dirs = path_dirs();
    if let Some(home) = dirs::home_dir() {
        for d in USER_BIN_DIRS {
            dirs.push(home.join(d));
        }
    }
    find_in(dirs, program)
}

#[cfg(not(windows))]
const USER_BIN_DIRS: &[&str] = &[".local/bin", ".opencode/bin", ".cargo/bin", ".bun/bin", ".npm-global/bin", ".volta/bin"];
#[cfg(windows)]
const USER_BIN_DIRS: &[&str] =
    &["AppData/Roaming/npm", ".local/bin", ".opencode/bin", ".cargo/bin", ".bun/bin", "AppData/Local/Volta/bin", "scoop/shims"];

/// A terminal the hub knows how to drive.
#[cfg(any(not(windows), test))]
struct KnownTerminal {
    name: &'static str,
    /// Argument that introduces the command to run.
    prefix: &'static [&'static str],
    /// Flag that sets the window title (empty: the terminal has none, or konsole's own).
    title: &'static str,
}

#[cfg(any(not(windows), test))]
const fn known(name: &'static str, prefix: &'static [&'static str], title: &'static str) -> KnownTerminal {
    KnownTerminal { name, prefix, title }
}

/// Known terminals, in the order they are looked for.
#[cfg(any(not(windows), test))]
const KNOWN_TERMINALS: &[KnownTerminal] = &[
    known("konsole", &["-e"], ""),
    known("gnome-terminal", &["--"], "--title"),
    known("kgx", &["--"], "--title"),
    known("ptyxis", &["--"], ""),
    known("xfce4-terminal", &["-x"], "--title"),
    known("mate-terminal", &["-x"], "--title"),
    known("tilix", &["-e"], "--title"),
    known("alacritty", &["-e"], "--title"),
    known("kitty", &[], "--title"),
    known("foot", &[], "--title"),
    known("wezterm", &["start", "--"], ""),
    known("xterm", &["-e"], "-T"),
    known("x-terminal-emulator", &["-e"], ""),
];

/// The entry for terminal `term` (name or path), if it is on the list.
#[cfg(any(not(windows), test))]
fn known_terminal(term: &str) -> Option<&'static KnownTerminal> {
    let name = Path::new(term).file_name().and_then(|n| n.to_str()).unwrap_or(term);
    KNOWN_TERMINALS.iter().find(|t| t.name == name)
}

/// Argument that introduces the command for terminal `term` (name or path). A terminal not
/// on the list gets `-e`, the most common one.
#[cfg(any(not(windows), test))]
fn terminal_prefix(term: &str) -> &'static [&'static str] {
    known_terminal(term).map_or(&["-e"], |t| t.prefix)
}

/// Arguments that give the window of terminal `term` the title `title` (none for a
/// terminal without such a flag, or one not on the list).
#[cfg(any(not(windows), test))]
fn terminal_title_args(term: &str, title: &str) -> Vec<String> {
    let Some(t) = known_terminal(term).filter(|_| !title.is_empty()) else { return Vec::new() };
    match t.name {
        "konsole" => vec!["-p".into(), format!("tabtitle={title}")],
        _ if t.title.is_empty() => Vec::new(),
        _ => vec![t.title.into(), title.into()],
    }
}

/// Available terminal and the argument that introduces the command to run.
#[cfg(not(windows))]
fn terminal() -> Option<(String, &'static [&'static str])> {
    if let Ok(t) = std::env::var("TERMINAL")
        && (which(&t) || Path::new(&t).is_file()) {
            let prefix = terminal_prefix(&t);
            return Some((t, prefix));
        }
    KNOWN_TERMINALS.iter().find(|t| which(t.name)).map(|t| (t.name.to_string(), t.prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_programs_outside_the_allowlist() {
        let e = Exec::new("test", &["docker".into()]);
        assert!(e.check("docker").is_ok());
        assert!(e.check("rm").is_err());
        assert!(e.open_terminal("x", &["bash"]).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn returns_output_error_and_missing_program() {
        let limit = Duration::from_secs(5);
        assert_eq!(run_timeout("sh", &["-c", "echo hi"], limit).unwrap(), "hi\n");
        assert_eq!(
            run_timeout("sh", &["-c", "echo bad >&2; exit 3"], limit),
            Err(ExecError::Failed("bad".into()))
        );
        let missing = run_timeout("miniutils-program-that-does-not-exist", &[""; 0], limit).unwrap_err();
        assert!(missing.not_found());
        assert_eq!(missing.to_string(), "miniutils-program-that-does-not-exist is not installed");
        // exits with an error, but the stdout comes along
        let out = output_timeout("sh", &["-c", "echo part; exit 1"], limit).unwrap();
        assert!(!out.success);
        assert_eq!(out.stdout, "part\n");
    }

    #[test]
    #[cfg(unix)]
    fn kills_what_runs_past_the_deadline() {
        let start = Instant::now();
        // `sleep` is a grandchild (child of sh): it dies too, otherwise it would hold the pipe for 30 s
        let r = run_timeout("sh", &["-c", "sleep 30; echo late"], Duration::from_millis(200));
        assert!(matches!(r, Err(ExecError::Timeout { .. })), "{r:?}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    #[cfg(windows)]
    fn windows_returns_output_error_and_missing_program() {
        let limit = Duration::from_secs(10);
        assert_eq!(run_timeout("cmd", &["/c", "echo hi"], limit).unwrap().trim(), "hi");
        let out = output_timeout("cmd", &["/c", "echo part& exit 1"], limit).unwrap();
        assert!(!out.success);
        assert_eq!(out.stdout.trim(), "part");
        assert!(run_timeout("miniutils-program-that-does-not-exist", &[""; 0], limit).unwrap_err().not_found());
    }

    #[test]
    #[cfg(windows)]
    fn windows_kills_the_tree_that_runs_past_the_deadline() {
        let start = Instant::now();
        // `ping` is a grandchild (child of cmd): it dies too, otherwise it would hold the pipe for 30 s
        let r = run_timeout("cmd", &["/c", "ping -n 30 127.0.0.1"], Duration::from_millis(500));
        assert!(matches!(r, Err(ExecError::Timeout { .. })), "{r:?}");
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn program_names_on_windows() {
        let ext = ".COM;.EXE;.BAT;.CMD";
        assert_eq!(windows_file_names("docker", ext), ["docker.com", "docker.exe", "docker.bat", "docker.cmd"]);
        // with an extension, the name itself comes first
        assert_eq!(windows_file_names("wt.exe", ".EXE")[0], "wt.exe");
        // a name with a dot that is not an executable extension still gets the PATHEXT ones
        assert!(windows_file_names("python3.12", ext).contains(&"python3.12.exe".to_string()));
        assert_eq!(windows_file_names("x", ";;"), Vec::<String>::new());
    }

    #[test]
    fn outside_terminal_uses_the_right_separator() {
        assert_eq!(terminal_prefix("kitty"), &[] as &[&str]);
        assert_eq!(terminal_prefix("/usr/bin/gnome-terminal"), &["--"]);
        assert_eq!(terminal_prefix("wezterm"), &["start", "--"]);
        assert_eq!(terminal_prefix("st"), &["-e"]);
    }

    #[test]
    fn outside_terminal_gets_the_title_flag() {
        assert_eq!(terminal_title_args("/usr/bin/kitty", "Logs"), ["--title", "Logs"]);
        assert_eq!(terminal_title_args("xterm", "Logs"), ["-T", "Logs"]);
        assert_eq!(terminal_title_args("konsole", "Logs"), ["-p", "tabtitle=Logs"]);
        assert!(terminal_title_args("wezterm", "Logs").is_empty());
        assert!(terminal_title_args("st", "Logs").is_empty());
        assert!(terminal_title_args("kitty", "").is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn runaway_output_is_cut() {
        let r = run_timeout("sh", &["-c", "yes | head -c 20000000"], Duration::from_secs(20));
        assert!(matches!(&r, Err(ExecError::Failed(m)) if m.contains("more than 8 MiB")), "{r:?}");
    }

    #[test]
    fn capped_reader_keeps_what_fits() {
        assert_eq!(read_capped(&mut &b"abc"[..], 3), Some(b"abc".to_vec()));
        assert_eq!(read_capped(&mut &b"abcd"[..], 3), None);
    }

    #[test]
    #[cfg(unix)]
    fn large_output_does_not_block() {
        // more than a pipe buffer (64 KiB) on both channels
        let out = run_timeout("sh", &["-c", "head -c 300000 /dev/zero | tr '\\0' a; head -c 300000 /dev/zero | tr '\\0' b >&2"], Duration::from_secs(10)).unwrap();
        assert_eq!(out.len(), 300000);
    }
}
