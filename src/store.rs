//! Loja de plugins: índice, instalação, atualização e remoção.
//!
//! Segue o modelo do marketplace do Omarchy: um índice (`registry.json`) lista os plugins,
//! cada um num repositório git. Diferenças de propósito:
//! - o índice fixa o **commit** revisado, e a instalação confere que veio exatamente ele
//!   (o Omarchy clona o HEAD, que pode ter mudado depois da revisão);
//! - o índice é **assinado** (ed25519, `registry.json.sig` ao lado): com a chave pública
//!   configurada, índice adulterado é recusado. Sem chave, só um índice local é aceito;
//! - o plugin não roda código nativo: o host executa o manifesto e, nos plugins com tela,
//!   interpreta o `.slint` e o script Rhai isolados (ver `apps/plugin.rs`); só os arquivos
//!   que `plugins::files` aceita são copiados;
//! - as permissões que o índice anuncia (programas, domínios da rede e provedores lidos)
//!   têm de bater com as do manifesto, e aparecem antes de instalar (numa atualização, as
//!   novas aparecem destacadas).
//!
//! Linha de comando em `USAGE`; a janela usa a mesma lógica (aba Loja, `hub/store.rs`).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ring::signature::{self, KeyPair};
use serde::{Deserialize, Serialize};

use crate::core::config::Config;
use crate::core::exec;
use crate::core::plugins::{self, Plugin};

pub const USAGE: &str = "\
loja de plugins:
  miniutils --loja [índice]                     lista os plugins e o estado de cada um
  miniutils --instalar <id> [índice] [--sim]    instala (pergunta antes, sem --sim)
  miniutils --atualizar <id> [índice] [--sim]   atualiza para o commit do índice
  miniutils --remover <id>                      remove um plugin instalado
  miniutils --plugins                           lista os instalados
  --chave <hex>     chave pública que assina o índice (padrão: store_key da config)
Sem índice, usa o store_index da config ou, se ele estiver vazio, a loja oficial
(github.com/pedroluizmossi/miniutils-store). O índice é um registry.json (caminho ou
https://), com a assinatura em registry.json.sig.

quem mantém um índice:
  miniutils --loja-chave <arquivo>              gera um par de chaves (imprime a pública)
  miniutils --loja-assinar <registry.json> <arquivo>   grava registry.json.sig

O miniutils aberto relê os plugins sozinho depois de instalar, atualizar ou remover.";

/// Prazo do `git clone`/`checkout`.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// A loja oficial: vale quando a config não diz outra (`store_index` vazio). O índice e a
/// chave são os de https://github.com/pedroluizmossi/miniutils-store.
pub const OFFICIAL_INDEX: &str = "https://raw.githubusercontent.com/pedroluizmossi/miniutils-store/main/registry.json";
pub const OFFICIAL_KEY: &str = "070dba03dd19902eeed1d02573272be1fa3fc263f4a8eda3cf10b3beca98464e";

/// De onde vem o índice e com que chave conferir.
#[derive(Debug, Clone, Default)]
pub struct Source {
    pub index: String,
    /// Chave pública ed25519 (hex). Vazia = sem conferência (só índice local).
    pub key: String,
}

impl Source {
    /// O índice e a chave da configuração; sem índice, a loja oficial.
    pub fn from_config(cfg: &Config) -> Source {
        let index = cfg.store_index.trim();
        Source { index: if index.is_empty() { OFFICIAL_INDEX } else { index }.to_string(), key: cfg.store_key.trim().to_string() }
            .with_official_key()
    }

    /// O índice oficial sem chave informada usa a chave oficial.
    fn with_official_key(mut self) -> Source {
        if self.key.is_empty() && self.index == OFFICIAL_INDEX {
            self.key = OFFICIAL_KEY.to_string();
        }
        self
    }
}

#[derive(Debug, Deserialize)]
pub struct Index {
    pub schema: u32,
    pub plugins: Vec<Entry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    pub license: String,
    pub version: String,
    /// Repositório git: `https://…` ou uma pasta local (para testes).
    pub repo: String,
    /// Commit revisado (SHA completo, 40 caracteres).
    pub commit: String,
    /// Subpasta do plugin no repositório (um repo pode ter vários).
    #[serde(default)]
    pub path: String,
    /// Programas que o plugin executa (tem de bater com o manifesto).
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Domínios que o plugin consulta (`[permissions] net`).
    #[serde(default)]
    pub net: Vec<String>,
    /// Provedores que o plugin lê (`uses`).
    #[serde(default)]
    pub uses: Vec<String>,
}

impl Entry {
    /// "executa: ps · rede: api.x.com · lê: sys".
    pub fn grants_text(&self) -> String {
        plugins::describe_grants(&self.permissions, &self.net, &self.uses)
    }
}

/// Cada permissão com o tipo na frente ("ps", "rede: api.x.com", "lê: sys"), para comparar
/// versões.
fn grant_list(exec: &[String], net: &[String], uses: &[String]) -> Vec<String> {
    exec.iter()
        .cloned()
        .chain(net.iter().map(|h| format!("rede: {h}")))
        .chain(uses.iter().map(|t| format!("lê: {t}")))
        .collect()
}

/// O que fica gravado ao lado do plugin instalado.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installed {
    pub id: String,
    pub version: String,
    pub repo: String,
    pub commit: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub net: Vec<String>,
    #[serde(default)]
    pub uses: Vec<String>,
}

/// Como o índice foi conferido.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// Assinatura válida com a chave configurada.
    Signed,
    /// Sem chave configurada: índice local, aceito sem conferência.
    Unsigned,
}

/// Um plugin do índice e a situação dele aqui.
#[derive(Debug, Clone)]
pub struct Status {
    pub entry: Entry,
    pub installed: Option<Installed>,
}

impl Status {
    /// Instalado num commit diferente do que o índice fixa agora.
    pub fn has_update(&self) -> bool {
        self.installed.as_ref().is_some_and(|i| !i.commit.eq_ignore_ascii_case(&self.entry.commit))
    }

    /// Permissões que a versão do índice pede e a instalada não pedia ("who", "rede:
    /// api.x.com", "lê: agents").
    pub fn new_permissions(&self) -> Vec<String> {
        let had = self.installed.as_ref().map(|i| grant_list(&i.permissions, &i.net, &i.uses)).unwrap_or_default();
        let e = &self.entry;
        grant_list(&e.permissions, &e.net, &e.uses).into_iter().filter(|p| !had.contains(p)).collect()
    }
}

// ---- linha de comando ---------------------------------------------------------------

/// Atende os comandos da loja. `None` se `args` não é da loja; senão o código de saída.
pub fn run_cli(args: &[String]) -> Option<i32> {
    let cmd = args.first()?.as_str();
    if !matches!(
        cmd,
        "--loja" | "--instalar" | "--atualizar" | "--remover" | "--plugins" | "--loja-chave" | "--loja-assinar"
    ) {
        return None;
    }
    // `--chave <hex>` e `--sim` podem vir em qualquer posição
    let mut key = None;
    let mut yes = false;
    let mut pos: Vec<&str> = Vec::new();
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--chave" => key = it.next().cloned(),
            "--sim" => yes = true,
            s => pos.push(s),
        }
    }
    let cfg = Config::load();
    let source = |i: usize| -> Result<Source, String> {
        let mut s = Source::from_config(&cfg);
        if let Some(index) = pos.get(i) {
            s.index = index.to_string();
            // índice dado na linha de comando: a chave da config é de outro índice
            if s.index != Source::from_config(&cfg).index {
                s.key.clear();
            }
        }
        if let Some(k) = &key {
            s.key = k.clone();
        }
        Ok(s.with_official_key())
    };
    let root = plugins::dir();
    let (mut always, mut prompt) = (|_: &str| true, ask);
    let confirm: &mut dyn FnMut(&str) -> bool = if yes { &mut always } else { &mut prompt };
    let id = pos.first().copied();
    let result = match (cmd, id) {
        ("--loja", _) => source(0).and_then(|s| list(&s, &root)),
        ("--instalar", Some(id)) | ("--atualizar", Some(id)) => {
            source(1).and_then(|s| install(&s, id, &root, confirm)).map(|dir| {
                crate::instance::notify_plugins_changed();
                format!("instalado em {}", dir.display())
            })
        }
        ("--remover", Some(id)) => remove(id, &root).inspect(|_| crate::instance::notify_plugins_changed()),
        ("--plugins", _) => Ok(installed_list(&root)),
        ("--loja-chave", Some(path)) => keygen(Path::new(path)),
        ("--loja-assinar", Some(index)) => match pos.get(1) {
            Some(k) => sign(Path::new(index), Path::new(k)),
            None => Err(USAGE.to_string()),
        },
        _ => Err(USAGE.to_string()),
    };
    Some(match result {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    })
}

fn list(src: &Source, root: &Path) -> Result<String, String> {
    let (statuses, trust) = statuses(src, root)?;
    let mut out = format!(
        "{} plugin(s) em {} ({}):\n",
        statuses.len(),
        src.index,
        if trust == Trust::Signed { "assinatura conferida" } else { "índice local, sem assinatura conferida" }
    );
    for s in &statuses {
        let e = &s.entry;
        let state = match &s.installed {
            None => "não instalado".to_string(),
            Some(_) if s.has_update() => format!("instalado {} · atualização para {}", s.installed.as_ref().unwrap().version, e.version),
            Some(i) => format!("instalado {}", i.version),
        };
        out.push_str(&format!(
            "\n  {}  {} {}  ({}, {})  [{state}]\n    {}\n    {}\n",
            e.id,
            e.name,
            e.version,
            if e.author.is_empty() { "sem autor" } else { &e.author },
            e.license,
            e.description,
            e.grants_text()
        ));
    }
    Ok(out)
}

/// Pergunta no terminal se pode instalar.
fn ask(summary: &str) -> bool {
    print!("{summary}\nInstalar? [s/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    matches!(line.trim().to_lowercase().as_str(), "s" | "sim" | "y" | "yes")
}

// ---- índice e assinatura -----------------------------------------------------------

fn fetch(src: &str) -> Result<Vec<u8>, String> {
    if src.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.get(src).send().and_then(|r| r.error_for_status()).map_err(|e| format!("{src}: {e}"))?;
        let bytes = resp.bytes().map_err(|e| e.to_string())?;
        if bytes.len() > 4 << 20 {
            return Err(format!("{src}: índice grande demais"));
        }
        Ok(bytes.to_vec())
    } else {
        std::fs::read(src).map_err(|e| format!("{src}: {e}"))
    }
}

/// Lê o índice e confere a assinatura (`<índice>.sig`) com a chave da fonte.
pub fn load_index(src: &Source) -> Result<(Index, Trust), String> {
    // remoto sem chave: recusa antes de baixar
    if src.key.trim().is_empty() && src.index.starts_with("https://") {
        return Err(format!(
            "{}: índice remoto sem chave para conferir a assinatura (configure a chave da loja)",
            src.index
        ));
    }
    let bytes = fetch(&src.index)?;
    let trust = if src.key.trim().is_empty() {
        Trust::Unsigned
    } else {
        let key = from_hex(src.key.trim()).filter(|k| k.len() == 32).ok_or("chave da loja inválida (64 caracteres hex)")?;
        let sig_text = fetch(&format!("{}.sig", src.index)).map_err(|e| format!("sem assinatura do índice: {e}"))?;
        let sig = from_hex(String::from_utf8_lossy(&sig_text).trim()).ok_or("assinatura do índice ilegível")?;
        signature::UnparsedPublicKey::new(&signature::ED25519, &key)
            .verify(&bytes, &sig)
            .map_err(|_| format!("{}: a assinatura não confere (índice adulterado ou chave errada)", src.index))?;
        Trust::Signed
    };
    let index: Index =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: índice inválido: {e}", src.index))?;
    if index.schema != 1 {
        return Err(format!("{}: índice na versão {} (este miniutils entende a 1)", src.index, index.schema));
    }
    Ok((index, trust))
}

/// Gera um par de chaves ed25519: a privada vai para `path` (só o dono lê), a pública
/// sai na tela para pôr na config de quem usa a loja.
fn keygen(path: &Path) -> Result<String, String> {
    if path.exists() {
        return Err(format!("{} já existe: não sobrescrevo uma chave", path.display()));
    }
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| "não consegui gerar a chave")?;
    let pair = signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| "chave gerada inválida")?;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(to_hex(pkcs8.as_ref()).as_bytes()).map_err(|e| e.to_string())?;
    Ok(to_hex(pair.public_key().as_ref()))
}

/// Assina o índice com a chave privada de `key_path`; grava `<índice>.sig`.
fn sign(index: &Path, key_path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(key_path).map_err(|e| format!("{}: {e}", key_path.display()))?;
    let pkcs8 = from_hex(text.trim()).ok_or("arquivo de chave ilegível")?;
    let pair = signature::Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|_| "arquivo de chave inválido")?;
    let bytes = std::fs::read(index).map_err(|e| format!("{}: {e}", index.display()))?;
    // confere antes de assinar: não adianta assinar um índice que não abre
    serde_json::from_slice::<Index>(&bytes).map_err(|e| format!("{}: índice inválido: {e}", index.display()))?;
    let sig_path = PathBuf::from(format!("{}.sig", index.display()));
    std::fs::write(&sig_path, to_hex(pair.sign(&bytes).as_ref())).map_err(|e| e.to_string())?;
    Ok(format!("{}\nchave pública: {}", sig_path.display(), to_hex(pair.public_key().as_ref())))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

// ---- estado, instalação, remoção ---------------------------------------------------

/// O que está instalado em `root/<id>/`, se estiver.
pub fn installed(root: &Path, id: &str) -> Option<Installed> {
    let text = std::fs::read_to_string(root.join(id).join("installed.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Cada plugin do índice e se está instalado (e em que commit).
pub fn statuses(src: &Source, root: &Path) -> Result<(Vec<Status>, Trust), String> {
    let (index, trust) = load_index(src)?;
    let list = index
        .plugins
        .into_iter()
        .map(|entry| {
            let installed = installed(root, &entry.id);
            Status { entry, installed }
        })
        .collect();
    Ok((list, trust))
}

/// Instala (ou atualiza) o plugin `id` do índice em `root/<id>/`. `confirm` recebe o
/// resumo (nome, autor, commit, permissões) e decide. Devolve a pasta instalada.
pub fn install(src: &Source, id: &str, root: &Path, confirm: &mut dyn FnMut(&str) -> bool) -> Result<PathBuf, String> {
    let (index, trust) = load_index(src)?;
    let entry = index.plugins.iter().find(|e| e.id == id).ok_or_else(|| format!("\"{id}\" não está no índice"))?;
    plugins::validate_id(&entry.id)?;
    check_entry(entry)?;
    let previous = installed(root, id);
    if previous.as_ref().is_some_and(|p| p.commit.eq_ignore_ascii_case(&entry.commit)) {
        return Err(format!("{id} já está instalado nesta versão ({})", entry.version));
    }

    // baixa num lugar temporário, ao lado do destino (o `rename` final não cruza discos)
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let tmp = root.join(format!(".{id}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let result = fetch_and_install(entry, &trust, previous.as_ref(), root, &tmp, confirm);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Confere o que o índice diz antes de baixar qualquer coisa.
fn check_entry(e: &Entry) -> Result<(), String> {
    if e.commit.len() != 40 || !e.commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("{}: o índice precisa fixar o commit (SHA de 40 caracteres)", e.id));
    }
    // só https ou pasta local: nada de ext::, ssh com comando, file:// com truques
    let local = Path::new(&e.repo).is_absolute();
    if !(e.repo.starts_with("https://") || local) {
        return Err(format!("{}: repositório precisa ser https:// (ou uma pasta local, para testes)", e.id));
    }
    if e.path.contains("..") || e.path.starts_with('/') {
        return Err(format!("{}: path inválido no índice: {}", e.id, e.path));
    }
    if e.license.trim().is_empty() {
        return Err(format!("{}: o índice não declara a licença", e.id));
    }
    Ok(())
}

fn git(args: &[&str]) -> Result<String, String> {
    exec::run_timeout("git", args, GIT_TIMEOUT).map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))
}

fn fetch_and_install(
    e: &Entry,
    trust: &Trust,
    previous: Option<&Installed>,
    root: &Path,
    tmp: &Path,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<PathBuf, String> {
    let checkout = tmp.join("repo");
    let checkout_s = checkout.to_string_lossy().to_string();
    // sem hooks nem submódulos: o clone não roda nada do repositório
    git(&["clone", "--quiet", "--no-checkout", "-c", "core.hooksPath=/dev/null", "--", &e.repo, &checkout_s])?;
    git(&["-C", &checkout_s, "-c", "core.hooksPath=/dev/null", "checkout", "--quiet", "--detach", &e.commit])?;
    let head = git(&["-C", &checkout_s, "rev-parse", "HEAD"])?;
    if !head.trim().eq_ignore_ascii_case(&e.commit) {
        return Err(format!("{}: o repositório entregou o commit {}, não o {} do índice", e.id, head.trim(), e.commit));
    }

    let src = if e.path.is_empty() { checkout.clone() } else { checkout.join(&e.path) };
    let text = std::fs::read_to_string(src.join("manifest.toml")).map_err(|err| format!("{}: sem manifest.toml ({err})", e.id))?;
    let plugin = Plugin::parse(&text).map_err(|err| format!("{}: manifesto inválido: {err}", e.id))?;
    check_matches(e, &plugin)?;
    if let Some(ui) = &plugin.ui {
        // a tela e o script também precisam abrir antes de instalar
        crate::apps::plugin::check_files(&src, ui).map_err(|err| format!("{}: {err}", e.id))?;
    }

    let new_perms: Vec<String> = match previous {
        Some(p) => {
            let had = grant_list(&p.permissions, &p.net, &p.uses);
            grant_list(&e.permissions, &e.net, &e.uses).into_iter().filter(|x| !had.contains(x)).collect()
        }
        None => Vec::new(),
    };
    let what = match (&plugin.run, &plugin.ui) {
        (_, Some(ui)) => format!("tela própria e script ({}, {})", ui.view, ui.script),
        (Some(run), None) => format!("a cada {} s: {}", run.every, run.command.join(" ")),
        (None, None) => String::new(),
    };
    let summary = format!(
        "\n{} {}  ({})\n  {}\n  autor: {}   licença: {}\n  repositório: {}\n  commit: {}\n  índice: {}\n  {}{}  ({what})\n{}",
        plugin.manifest.name,
        e.version,
        e.id,
        plugin.manifest.description,
        if e.author.is_empty() { "?" } else { &e.author },
        e.license,
        e.repo,
        e.commit,
        if *trust == Trust::Signed { "assinatura conferida" } else { "local, sem assinatura conferida" },
        match previous {
            Some(p) => format!("atualização de {} para {}\n  ", p.version, e.version),
            None => String::new(),
        },
        plugin.grants(),
        if new_perms.is_empty() { String::new() } else { format!("  ATENÇÃO, permissão nova: {}\n", new_perms.join(", ")) },
    );
    if !confirm(&summary) {
        return Err("instalação cancelada".into());
    }

    // só os arquivos permitidos, e só arquivos de verdade (nada de link)
    let staged = tmp.join("plugin");
    std::fs::create_dir_all(&staged).map_err(|err| err.to_string())?;
    for rel in plugins::files(&src).map_err(|err| format!("{}: {err}", e.id))? {
        let to = staged.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        std::fs::copy(src.join(&rel), &to).map_err(|err| format!("{}: {err}", rel.display()))?;
    }
    let installed = Installed {
        id: e.id.clone(),
        version: e.version.clone(),
        repo: e.repo.clone(),
        commit: e.commit.clone(),
        permissions: plugin.manifest.permissions.exec.clone(),
        net: plugin.manifest.permissions.net.clone(),
        uses: plugin.manifest.uses.iter().map(|t| t.name().to_string()).collect(),
    };
    let meta = serde_json::to_string_pretty(&installed).map_err(|err| err.to_string())?;
    std::fs::write(staged.join("installed.json"), meta).map_err(|err| err.to_string())?;

    // troca: a versão antiga só sai quando a nova está pronta ao lado
    let dest = root.join(&e.id);
    let old = tmp.join("antigo");
    if dest.exists() {
        std::fs::rename(&dest, &old).map_err(|err| format!("{}: {err}", dest.display()))?;
    }
    if let Err(err) = std::fs::rename(&staged, &dest) {
        let _ = std::fs::rename(&old, &dest);
        return Err(format!("{}: {err}", dest.display()));
    }
    Ok(dest)
}

/// O que o índice anuncia tem de ser o que o manifesto pede.
fn check_matches(e: &Entry, p: &Plugin) -> Result<(), String> {
    if p.manifest.id != e.id {
        return Err(format!("o manifesto diz id \"{}\", o índice \"{}\"", p.manifest.id, e.id));
    }
    let sorted = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v.dedup();
        v
    };
    let uses: Vec<String> = p.manifest.uses.iter().map(|t| t.name().to_string()).collect();
    let pairs = [
        ("executar", &e.permissions, &p.manifest.permissions.exec),
        ("consultar (net)", &e.net, &p.manifest.permissions.net),
        ("ler (uses)", &e.uses, &uses),
    ];
    for (what, want, got) in pairs {
        let (want, got) = (sorted(want), sorted(got));
        if want != got {
            return Err(format!("{}: o índice anuncia {what} {want:?}, mas o manifesto pede {got:?}", e.id));
        }
    }
    Ok(())
}

pub fn remove(id: &str, root: &Path) -> Result<String, String> {
    plugins::validate_id(id)?;
    let dir = root.join(id);
    if !dir.join("manifest.toml").is_file() {
        return Err(format!("\"{id}\" não está instalado"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // os dados que o script gravou vão junto
    let _ = std::fs::remove_file(plugins::data_file(root, id));
    Ok(format!("{id} removido"))
}

fn installed_list(root: &Path) -> String {
    let list = plugins::load_installed(root);
    if list.is_empty() {
        return format!("nenhum plugin instalado em {}", root.display());
    }
    let mut out = format!("instalados em {}:\n", root.display());
    for p in list {
        let (version, commit) = installed(root, &p.manifest.id)
            .map_or(("?".into(), "?".into()), |m| (m.version, m.commit.chars().take(12).collect::<String>()));
        out.push_str(&format!("  {}  {} {}  (commit {commit})\n", p.manifest.id, p.manifest.name, version));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plugins::tests::EXEMPLO;

    /// Repositório git de verdade com o plugin de exemplo e um índice apontando para ele.
    struct Loja {
        dir: PathBuf,
        commit: String,
    }

    impl Loja {
        fn new(nome: &str) -> Option<Loja> {
            if !exec::which("git") {
                return None;
            }
            let dir = std::env::temp_dir().join(format!("miniutils-loja-{nome}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let repo = dir.join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            std::fs::write(repo.join("manifest.toml"), EXEMPLO).unwrap();
            std::fs::write(repo.join("README.md"), "# exemplo").unwrap();
            std::fs::write(repo.join("instalar.sh"), "rm -rf ~").unwrap(); // não pode ser copiado
            let commit = Loja::commit(&repo, "v1");
            Some(Loja { dir, commit })
        }

        fn commit(repo: &Path, msg: &str) -> String {
            let r = repo.to_string_lossy().to_string();
            if !repo.join(".git").exists() {
                git(&["-C", &r, "init", "--quiet"]).unwrap();
            }
            git(&["-C", &r, "add", "."]).unwrap();
            git(&["-C", &r, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", msg]).unwrap();
            git(&["-C", &r, "rev-parse", "HEAD"]).unwrap().trim().to_string()
        }

        fn index(&self, commit: &str, version: &str, permissions: &str) -> Source {
            let path = self.dir.join("registry.json");
            let json = format!(
                r#"{{"schema":1,"plugins":[{{"id":"io.github.exemplo.uptime","name":"Tempo ligado","license":"MIT","version":"{version}","repo":"{}","commit":"{commit}","permissions":[{permissions}]}}]}}"#,
                self.dir.join("repo").display()
            );
            std::fs::write(&path, json).unwrap();
            Source { index: path.to_string_lossy().to_string(), key: String::new() }
        }
    }

    impl Drop for Loja {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn instala_no_commit_fixado_so_os_arquivos_permitidos() {
        let Some(loja) = Loja::new("ok") else { return };
        let src = loja.index(&loja.commit, "0.1.0", r#""uptime""#);
        let root = loja.dir.join("plugins");
        let mut resumo = String::new();
        let dest = install(&src, "io.github.exemplo.uptime", &root, &mut |s| {
            resumo = s.to_string();
            true
        })
        .unwrap();
        assert!(resumo.contains("executa: uptime"));
        assert!(dest.join("manifest.toml").is_file());
        assert!(dest.join("README.md").is_file());
        assert!(!dest.join("instalar.sh").exists(), "arquivo fora da lista não entra");
        assert_eq!(installed(&root, "io.github.exemplo.uptime").unwrap().commit, loja.commit);
        let instalados = plugins::load_installed(&root);
        assert_eq!(instalados.len(), 1);
        assert_eq!(instalados[0].manifest.name, "Tempo ligado");
        assert!(install(&src, "io.github.exemplo.uptime", &root, &mut |_| true).unwrap_err().contains("já está instalado"));
        assert!(remove("io.github.exemplo.uptime", &root).is_ok());
        assert!(plugins::load_installed(&root).is_empty());
    }

    #[test]
    fn atualiza_e_avisa_permissao_nova() {
        let Some(loja) = Loja::new("upd") else { return };
        let root = loja.dir.join("plugins");
        let id = "io.github.exemplo.uptime";
        install(&loja.index(&loja.commit, "0.1.0", r#""uptime""#), id, &root, &mut |_| true).unwrap();
        // versão nova pede mais um programa
        let repo = loja.dir.join("repo");
        let novo = EXEMPLO.replace(r#"exec = ["uptime"]"#, r#"exec = ["uptime", "who"]"#);
        std::fs::write(repo.join("manifest.toml"), novo).unwrap();
        let commit2 = Loja::commit(&repo, "v2");
        let src = loja.index(&commit2, "0.2.0", r#""uptime", "who""#);
        let (st, _) = statuses(&src, &root).unwrap();
        assert!(st[0].has_update());
        assert_eq!(st[0].new_permissions(), ["who"]);
        let mut resumo = String::new();
        install(&src, id, &root, &mut |s| {
            resumo = s.to_string();
            true
        })
        .unwrap();
        assert!(resumo.contains("atualização de 0.1.0 para 0.2.0"));
        assert!(resumo.contains("permissão nova: who"));
        assert_eq!(installed(&root, id).unwrap().version, "0.2.0");
    }

    #[test]
    fn recusa_commit_ou_permissao_que_nao_batem_e_respeita_o_nao() {
        let Some(loja) = Loja::new("ruim") else { return };
        let root = loja.dir.join("plugins");
        let id = "io.github.exemplo.uptime";
        // commit que não existe no repositório
        let outro = loja.index(&"a".repeat(40), "0.1.0", r#""uptime""#);
        assert!(install(&outro, id, &root, &mut |_| true).is_err());
        // índice escondendo uma permissão
        let mentira = loja.index(&loja.commit, "0.1.0", "");
        assert!(install(&mentira, id, &root, &mut |_| true).unwrap_err().contains("anuncia"));
        // commit curto (não fixado)
        let curto = loja.index("abc123", "0.1.0", r#""uptime""#);
        assert!(install(&curto, id, &root, &mut |_| true).unwrap_err().contains("40 caracteres"));
        // o usuário disse não
        let ok = loja.index(&loja.commit, "0.1.0", r#""uptime""#);
        assert!(install(&ok, id, &root, &mut |_| false).unwrap_err().contains("cancelada"));
        assert!(plugins::load_installed(&root).is_empty());
    }

    #[test]
    fn sem_indice_na_config_vale_a_loja_oficial() {
        let mut cfg = Config::default();
        let s = Source::from_config(&cfg);
        assert_eq!((s.index.as_str(), s.key.as_str()), (OFFICIAL_INDEX, OFFICIAL_KEY));
        // outra loja: a chave oficial não vai junto
        cfg.store_index = "/tmp/outra/registry.json".into();
        assert_eq!(Source::from_config(&cfg).key, "");
        cfg.store_key = "ab".repeat(32);
        assert_eq!(Source::from_config(&cfg).key, "ab".repeat(32));
    }

    #[test]
    fn assinatura_do_indice() {
        let dir = std::env::temp_dir().join(format!("miniutils-assina-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let index = dir.join("registry.json");
        std::fs::write(&index, r#"{"schema":1,"plugins":[]}"#).unwrap();
        let key_file = dir.join("chave");
        let public = keygen(&key_file).unwrap();
        assert!(keygen(&key_file).is_err(), "não sobrescreve a chave");
        sign(&index, &key_file).unwrap();
        let src = Source { index: index.to_string_lossy().to_string(), key: public.clone() };
        assert_eq!(load_index(&src).unwrap().1, Trust::Signed);
        // adulterado depois de assinar
        std::fs::write(&index, r#"{"schema":1,"plugins":[ ]}"#).unwrap();
        assert!(load_index(&src).unwrap_err().contains("não confere"));
        // chave de outra loja
        let outra = Source { key: "11".repeat(32), ..src.clone() };
        assert!(load_index(&outra).is_err());
        // remoto sem chave: recusado antes de baixar
        let remoto = Source { index: "https://example.invalid/registry.json".into(), key: String::new() };
        assert!(load_index(&remoto).unwrap_err().contains("sem chave"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
