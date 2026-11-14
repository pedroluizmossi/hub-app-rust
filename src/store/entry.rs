//! `--store-entry`: the store maintainer adds or updates an app's entry in `registry.json`
//! from its repository and tag, without editing the JSON by hand.
//!
//! Everything comes from the code at that commit and from the tag's Release, checked the
//! same way installation checks it (see `fetch_and_install`):
//! - the commit is the one the tag points to in the clone (no hooks), and the manifest there
//!   gives id, name, description and grants (`permissions`, `net`, `uses`, `sqlite`,
//!   `full_access`);
//! - for an external app, the executables come from the `registry-entry.json` of the tag's
//!   Release (what miniutils-apps' release workflow publishes), which must point to the same
//!   commit; each executable is downloaded and its SHA-256 checked here;
//! - author and license are kept from the existing entry; for a new entry, the repository
//!   owner and the license from the `LICENSE` file (or `--license`).
//!
//! It does not sign: the report shows what changed (version, commit, new grants and the link
//! to compare the code) for review before `--loja-assinar`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{check_entry, check_matches, download_verified, fetch, git, grant_list, Binary, Entry, Index};
use crate::core::appdata::Kind;
use crate::core::plugins::{self, Plugin};

/// `--store-entry` options besides the index, the repository and the tag.
#[derive(Debug, Default)]
pub struct Opts {
    /// App subfolder in the repository. Without it: the tag prefix (`repos-v0.2.0` → `repos`)
    /// when that folder has a manifest, otherwise the root.
    pub path: Option<String>,
    /// Version, when the tag does not end in `v<version>`.
    pub version: Option<String>,
    /// License (SPDX), when the entry is new and `LICENSE` does not tell.
    pub license: Option<String>,
}

/// Adds or updates in `index_path` the app from `repo` at tag (or branch, or commit)
/// `git_ref`. Returns the report of what changed.
pub fn run(index_path: &Path, repo: &str, git_ref: &str, opts: &Opts) -> Result<String, String> {
    let repo = normalize_repo(repo)?;
    let text = std::fs::read_to_string(index_path).map_err(|e| format!("{}: {e}", index_path.display()))?;
    let mut index: Index = serde_json::from_str(&text).map_err(|e| format!("{}: índice inválido: {e}", index_path.display()))?;
    if index.schema != 1 {
        return Err(format!("{}: índice na versão {} (este miniutils entende a 1)", index_path.display(), index.schema));
    }
    let tmp = std::env::temp_dir().join(format!("miniutils-store-entry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let result = build(&index, &repo, git_ref, opts, &tmp);
    let _ = std::fs::remove_dir_all(&tmp);
    let (entry, report) = result?;

    // in place of the old one (so the index diff touches only it), or at the end
    match index.plugins.iter_mut().find(|e| e.id == entry.id) {
        Some(slot) => *slot = entry,
        None => index.plugins.push(entry),
    }
    let mut json = serde_json::to_string_pretty(&index).map_err(|e| e.to_string())?;
    json.push('\n');
    std::fs::write(index_path, json).map_err(|e| format!("{}: {e}", index_path.display()))?;
    Ok(format!("{report}{} atualizado; revise e assine com --loja-assinar", index_path.display()))
}

/// Builds the entry from the clone and the Release, plus the report for the reviewer.
fn build(index: &Index, repo: &str, git_ref: &str, opts: &Opts, tmp: &Path) -> Result<(Entry, String), String> {
    let version = match &opts.version {
        Some(v) => v.trim().to_string(),
        None => version_from_ref(git_ref).ok_or_else(|| {
            format!("{git_ref}: a tag não termina em v<versão> (ex.: repos-v0.2.0); informe a versão com --app-version")
        })?,
    };

    // the same clone as installation: no hooks, only the requested commit
    let checkout = tmp.join("repo");
    let checkout_s = checkout.to_string_lossy().to_string();
    git(&["clone", "--quiet", "--no-checkout", "-c", "core.hooksPath=/dev/null", "--", repo, &checkout_s])?;
    let commit = resolve(&checkout_s, git_ref)?;
    git(&["-C", &checkout_s, "-c", "core.hooksPath=/dev/null", "checkout", "--quiet", "--detach", &commit])?;

    let path = match &opts.path {
        Some(p) => p.trim_matches('/').to_string(),
        None => tag_prefix(git_ref)
            .filter(|p| checkout.join(p).join("manifest.toml").is_file())
            .map(str::to_string)
            .unwrap_or_default(),
    };
    let src = if path.is_empty() { checkout.clone() } else { checkout.join(&path) };
    let text = std::fs::read_to_string(src.join("manifest.toml"))
        .map_err(|e| format!("{repo} ({git_ref}): sem {} ({e}); a subpasta do app vai em --path", manifest_path(&path)))?;
    let plugin = Plugin::parse(&text).map_err(|e| format!("{}: manifesto inválido: {e}", manifest_path(&path)))?;
    let id = plugin.manifest.id.clone();
    plugins::validate_id(&id)?;

    let old = index.plugins.iter().find(|e| e.id == id);
    if old.is_some_and(|o| o.commit.eq_ignore_ascii_case(&commit)) {
        return Err(format!("{id} já está no índice nesse commit ({commit})"));
    }
    let license = match (&opts.license, old) {
        (Some(l), _) => l.trim().to_string(),
        (None, Some(o)) => o.license.clone(),
        (None, None) => detect_license(&[&src, &checkout])
            .ok_or("não reconheci a licença pelo arquivo LICENSE; informe com --license (ex.: MIT)")?
            .to_string(),
    };
    let author = match old {
        Some(o) if !o.author.is_empty() => o.author.clone(),
        _ => owner(repo).unwrap_or_default(),
    };
    let mut entry = Entry {
        id: id.clone(),
        name: plugin.manifest.name.clone(),
        description: plugin.manifest.description.clone(),
        author,
        license,
        version,
        repo: repo.to_string(),
        path,
        commit,
        permissions: plugin.manifest.permissions.exec.clone(),
        net: plugin.manifest.permissions.net.clone(),
        uses: plugin.manifest.uses.iter().map(|t| t.name().to_string()).collect(),
        full_access: plugin.full_access(),
        sqlite: plugin.data().has(Kind::Sqlite),
        binaries: BTreeMap::new(),
    };
    // the view (and the script) must also open, as in installation
    if let Some(ui) = &plugin.ui {
        crate::apps::plugin::check_files(&src, ui).map_err(|e| format!("{id}: {e}"))?;
    }
    if plugin.app.is_some() {
        crate::apps::plugin::check_view(&src).map_err(|e| format!("{id}: {e}"))?;
        entry.binaries = release_binaries(&entry, git_ref, tmp)?;
    }
    check_entry(&entry)?;
    check_matches(&entry, &plugin)?;

    let report = report(old, &entry);
    Ok((entry, report))
}

/// The tag's Release executables: the `registry-entry.json` the workflow publishes, checked
/// against the commit, downloading each executable to check its SHA-256.
fn release_binaries(entry: &Entry, tag: &str, tmp: &Path) -> Result<BTreeMap<String, Binary>, String> {
    let base = format!("{}/releases/download/{tag}/", entry.repo);
    let url = format!("{base}registry-entry.json");
    let bytes = fetch(&url).map_err(|e| {
        format!("{}: app externo precisa do Release da tag {tag} com registry-entry.json (o workflow de release gera): {e}", entry.id)
    })?;
    let release: Entry = serde_json::from_slice(&bytes).map_err(|e| format!("{url}: {e}"))?;
    if release.id != entry.id || release.path != entry.path {
        return Err(format!("{url}: é de outro app ({} em \"{}\")", release.id, release.path));
    }
    if !release.commit.eq_ignore_ascii_case(&entry.commit) {
        return Err(format!(
            "{url}: o Release foi compilado do commit {}, mas a tag aponta para {}",
            release.commit, entry.commit
        ));
    }
    if release.binaries.is_empty() {
        return Err(format!("{url}: sem executáveis"));
    }
    for (os, b) in &release.binaries {
        // only files from this Release: no outside address
        if !b.url.starts_with(&base) {
            return Err(format!("{url}: o executável de {os} não é deste Release ({})", b.url));
        }
        download_verified(b, &tmp.join(format!("bin-{os}"))).map_err(|e| format!("{}: {os}: {e}", entry.id))?;
    }
    Ok(release.binaries)
}

/// The reviewer's report: version, commit, compare link and new grants.
fn report(old: Option<&Entry>, e: &Entry) -> String {
    let short = |c: &str| c.chars().take(12).collect::<String>();
    // `id:` and `version:` first and plain: the store's tools/publish.sh reads them
    let mut out = format!("id: {}\nversion: {}\n", e.id, e.version);
    match old {
        Some(o) => {
            out += &format!("{} {} → {}\n", e.name, o.version, e.version);
            out += &format!("commit: {} → {}\n", short(&o.commit), short(&e.commit));
            if o.repo == e.repo && e.repo.starts_with("https://github.com/") {
                out += &format!("comparar: {}/compare/{}...{}\n", e.repo, o.commit, e.commit);
            }
        }
        None => {
            out += &format!("{} {} (novo no índice)\n", e.name, e.version);
            out += &format!("commit: {}\n", e.commit);
        }
    }
    let folder = if e.path.is_empty() { String::new() } else { format!(" (pasta {})", e.path) };
    out += &format!("repositório: {}{folder}\n", e.repo);
    out += &format!("autor: {}   licença: {}\n", e.author, e.license);
    out += &format!("permissões: {}\n", e.grants_text());
    let had = old.map(|o| grant_list(&o.permissions, &o.net, &o.uses, o.sqlite, o.full_access)).unwrap_or_default();
    let new: Vec<String> = grant_list(&e.permissions, &e.net, &e.uses, e.sqlite, e.full_access)
        .into_iter()
        .filter(|g| !had.contains(g))
        .collect();
    if old.is_some() && !new.is_empty() {
        out += &format!("ATENÇÃO, permissão nova: {}\n", new.join(", "));
    }
    if !e.binaries.is_empty() {
        let oses: Vec<&str> = e.binaries.keys().map(String::as_str).collect();
        out += &format!("executáveis conferidos (SHA-256 do Release): {}\n", oses.join(", "));
    }
    out
}

/// The full commit SHA of a tag, remote branch or commit.
fn resolve(checkout: &str, git_ref: &str) -> Result<String, String> {
    if git_ref.starts_with('-') {
        return Err(format!("{git_ref}: referência inválida"));
    }
    let mut last = String::new();
    for candidate in [format!("refs/tags/{git_ref}"), format!("refs/remotes/origin/{git_ref}"), git_ref.to_string()] {
        // `rev-list -n 1` peels an annotated tag down to its commit
        match git(&["-C", checkout, "rev-list", "-n", "1", &candidate, "--"]) {
            Ok(sha) if sha.trim().len() == 40 => return Ok(sha.trim().to_lowercase()),
            Ok(other) => last = other.trim().to_string(),
            Err(e) => last = e,
        }
    }
    Err(format!("{git_ref}: não há essa tag, branch ou commit no repositório (a tag foi enviada?) [{last}]"))
}

/// `owner/repo` becomes `https://github.com/owner/repo`; strips a trailing `.git` and `/`.
/// An absolute path (local repository, for tests) is kept as is.
fn normalize_repo(repo: &str) -> Result<String, String> {
    let r = repo.trim().trim_end_matches('/');
    let r = r.strip_suffix(".git").unwrap_or(r);
    if r.starts_with("https://") || Path::new(r).is_absolute() {
        return Ok(r.to_string());
    }
    let parts: Vec<&str> = r.split('/').collect();
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if parts.len() == 2 && parts.iter().all(|p| ok(p)) {
        return Ok(format!("https://github.com/{r}"));
    }
    Err(format!("{repo}: use https://… ou dono/repositório (no GitHub)"))
}

/// The repository owner in the address (`https://github.com/owner/repo` → `owner`).
fn owner(repo: &str) -> Option<String> {
    let rest = repo.strip_prefix("https://")?;
    let mut parts = rest.split('/');
    parts.next()?;
    parts.next().filter(|s| !s.is_empty()).map(str::to_string)
}

/// The version at the end of the tag: `repos-v0.2.0` and `v0.2.0` → `0.2.0`.
fn version_from_ref(git_ref: &str) -> Option<String> {
    let (_, v) = git_ref.rsplit_once('v')?;
    let starts_digit = v.chars().next().is_some_and(|c| c.is_ascii_digit());
    let before_ok = git_ref.len() == v.len() + 1 || git_ref[..git_ref.len() - v.len() - 1].ends_with('-');
    (starts_digit && before_ok && v.contains('.')).then(|| v.to_string())
}

/// The app folder from a miniutils-apps tag: `repos-v0.2.0` → `repos`.
fn tag_prefix(git_ref: &str) -> Option<&str> {
    let i = git_ref.rfind("-v")?;
    let p = &git_ref[..i];
    (!p.is_empty() && !p.contains(['/', '\\', '.'])).then_some(p)
}

fn manifest_path(path: &str) -> String {
    if path.is_empty() { "manifest.toml".into() } else { format!("{path}/manifest.toml") }
}

/// The license (SPDX) from the start of the `LICENSE` file in the app folder or the root.
fn detect_license(dirs: &[&PathBuf]) -> Option<&'static str> {
    let text = dirs
        .iter()
        .flat_map(|d| ["LICENSE", "LICENSE.md", "LICENSE.txt", "LICENCE"].map(|n| d.join(n)))
        .find_map(|p| std::fs::read_to_string(p).ok())?;
    license_of(&text)
}

fn license_of(text: &str) -> Option<&'static str> {
    let head: String = text.chars().take(600).collect::<String>().to_lowercase();
    let known = [
        ("mit license", "MIT"),
        ("permission is hereby granted, free of charge", "MIT"),
        ("apache license", "Apache-2.0"),
        ("mozilla public license version 2.0", "MPL-2.0"),
        ("gnu lesser general public license", "LGPL-3.0"),
        ("gnu affero general public license", "AGPL-3.0"),
        ("gnu general public license", "GPL-3.0"),
        ("bsd 3-clause", "BSD-3-Clause"),
        ("bsd 2-clause", "BSD-2-Clause"),
        ("this is free and unencumbered software", "Unlicense"),
    ];
    known.iter().find(|(needle, _)| head.contains(needle)).map(|(_, spdx)| *spdx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_folder_from_tag() {
        assert_eq!(version_from_ref("repos-v0.2.0").as_deref(), Some("0.2.0"));
        assert_eq!(version_from_ref("v1.10.3").as_deref(), Some("1.10.3"));
        assert_eq!(version_from_ref("dev-tools-v0.1.0-rc1").as_deref(), Some("0.1.0-rc1"));
        assert_eq!(version_from_ref("main"), None);
        assert_eq!(version_from_ref("dev"), None);
        assert_eq!(version_from_ref("abcv1.0"), None);
        assert_eq!(tag_prefix("repos-v0.2.0"), Some("repos"));
        assert_eq!(tag_prefix("v0.2.0"), None);
        assert_eq!(tag_prefix("../x-v1.0"), None);
    }

    #[test]
    fn short_repo_becomes_github() {
        assert_eq!(normalize_repo("pedroluizmossi/miniutils-apps").unwrap(), "https://github.com/pedroluizmossi/miniutils-apps");
        assert_eq!(normalize_repo("https://github.com/a/b.git/").unwrap(), "https://github.com/a/b");
        assert!(normalize_repo("git@github.com:a/b").is_err());
        assert!(normalize_repo("a/b/c").is_err());
        assert!(normalize_repo("ext::sh -c x").is_err());
        assert_eq!(owner("https://github.com/pedroluizmossi/x").as_deref(), Some("pedroluizmossi"));
    }

    #[test]
    fn license_from_file() {
        assert_eq!(license_of("MIT License\n\nCopyright (c) 2026"), Some("MIT"));
        assert_eq!(license_of("                                 Apache License\n Version 2.0"), Some("Apache-2.0"));
        assert_eq!(license_of("GNU GENERAL PUBLIC LICENSE\nVersion 3"), Some("GPL-3.0"));
        assert_eq!(license_of("GNU LESSER GENERAL PUBLIC LICENSE"), Some("LGPL-3.0"));
        assert_eq!(license_of("all rights reserved"), None);
    }

    /// The rewritten index has the hand-written shape: fields in the usual order and no empty
    /// ones, so the diff shows only the entry that changed.
    #[test]
    fn rewriting_the_index_keeps_other_entries() {
        let original = r#"{
  "schema": 1,
  "plugins": [
    {
      "id": "io.github.a.pulso",
      "name": "Pulso",
      "description": "CPU ao vivo.",
      "author": "a",
      "license": "MIT",
      "version": "0.2.0",
      "repo": "https://github.com/a/pulso",
      "commit": "3f9a4b67321fd83df25634207c3aa7bd6a27fd79",
      "permissions": [
        "ps"
      ],
      "net": [
        "api.ipify.org"
      ],
      "uses": [
        "sys"
      ]
    },
    {
      "id": "io.github.a.repos",
      "name": "Repositórios",
      "description": "Git.",
      "author": "a",
      "license": "MIT",
      "version": "0.1.0",
      "repo": "https://github.com/a/apps",
      "path": "repos",
      "commit": "da97ee1366b6e637ddd041c5c1fd5ed97a9dd87e",
      "uses": [
        "agents"
      ],
      "full_access": true,
      "binaries": {
        "linux": {
          "url": "https://github.com/a/apps/releases/download/repos-v0.1.0/repos-linux-x86_64",
          "sha256": "6df474406028af137ba7d8a924e035abc181a374dafc26df67024c92fdce3b92"
        }
      }
    }
  ]
}
"#;
        let index: Index = serde_json::from_str(original).unwrap();
        let again = serde_json::to_string_pretty(&index).unwrap() + "\n";
        assert_eq!(again, original);
    }
}
