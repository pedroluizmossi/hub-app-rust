//! Configuração do usuário em `~/.config/miniutils/config.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::layout::{self, Rect};
use super::manifest::Manifest;

/// Nome do app (padrão do nome exibido, pasta da config, socket e binário em minúsculas).
pub const APP_NAME: &str = "MiniUtils";

/// Nome exibido atual, lido de outras threads (bandeja e notificações).
static DISPLAY_NAME: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// Nome que o usuário escolheu (`name` na config), ou `MiniUtils`.
pub fn display_name() -> String {
    let n = DISPLAY_NAME.read().map(|n| n.clone()).unwrap_or_default();
    if n.trim().is_empty() { APP_NAME.into() } else { n }
}

/// Versão atual do formato do arquivo. Ao mudar a estrutura, suba o número e
/// acrescente a migração correspondente em `migra`.
pub const VERSAO_ATUAL: u32 = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Versão do formato do arquivo (ausente = 0, o formato antigo).
    pub version: u32,
    /// Nome exibido na janela, na bandeja e nas notificações (o usuário pode trocar).
    pub name: String,
    /// Páginas do painel (a barra lateral), cada uma com a sua grade e os seus apps.
    pub pages: Vec<Page>,
    /// Visual da janela (ver `src/skin`).
    pub skin: String,
    /// Reduzir movimento: mascotes sem pulos nem tremores, e sem reações nem easter eggs.
    pub reduce_motion: bool,
    /// Quanto as mascotes conversam entre si: `"off"`, `"some"` ou `"lots"`.
    pub chatter: String,
    /// Onde ficam as páginas: `"side"` (barra lateral) ou `"top"` (abas no cabeçalho).
    pub nav: String,
    /// Mascote do ícone da bandeja: `"auto"` (a do app em pior estado) ou uma mascote.
    pub tray_icon: String,
    /// Tema do ícone da bandeja: `"auto"` (o do app mostrado) ou um tema.
    pub tray_theme: String,
    /// Posição e tamanho de cada app na grade da sua página, em células: `[layout."cyber.docker"]`.
    pub layout: BTreeMap<String, Rect>,
    /// Tamanho da janela em pixels lógicos (lembrado entre aberturas).
    pub window: Option<(u32, u32)>,
    /// Mini-apps ligados, na ordem das abas.
    pub enabled: Vec<String>,
    /// Configurações de cada mini-app: `[apps."cyber.docker"]`.
    pub apps: BTreeMap<String, toml::Table>,
    /// Índice da loja de plugins (`registry.json`: caminho ou https://). Vazio = a loja
    /// oficial (`store::OFFICIAL_INDEX`).
    pub store_index: String,
    /// Chave pública (ed25519, hex) que assina o índice da loja. Vazia = a oficial, se o
    /// índice for o oficial; para outro índice, sem conferência (só aceito se for local).
    pub store_key: String,
    /// Como salvar, conforme o arquivo lido (não vai para o TOML).
    #[serde(skip)]
    estado: Estado,
}

/// Uma página do painel: aparece na barra lateral e tem a própria grade.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Page {
    /// Nome mostrado na barra lateral.
    pub name: String,
    /// Mascote do ícone na barra lateral (chave de `mascot::sprites`).
    pub icon: String,
    /// Colunas da grade.
    pub columns: u32,
    /// Linhas da grade.
    pub rows: u32,
    /// Apps desta página, ligados ou não. Um app fica em uma página só.
    pub apps: Vec<String>,
}

impl Default for Page {
    fn default() -> Self {
        Page { name: "Geral".into(), icon: "neko".into(), columns: 2, rows: 1, apps: Vec::new() }
    }
}

/// O que fazer ao salvar, conforme o arquivo que foi lido.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
enum Estado {
    #[default]
    Normal,
    /// Arquivo de um MiniUtils mais novo: nunca é sobrescrito.
    SomenteLeitura,
    /// TOML quebrado: o original vai para `.quebrado` antes de salvar algo novo.
    Quebrado,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: VERSAO_ATUAL,
            name: APP_NAME.into(),
            pages: vec![Page {
                apps: vec!["cyber.docker".into(), "cyber.vitals".into(), "cyber.cd".into()],
                ..Page::default()
            }],
            skin: "neon".into(),
            reduce_motion: false,
            chatter: "some".into(),
            nav: "side".into(),
            tray_icon: "auto".into(),
            tray_theme: "auto".into(),
            window: None,
            layout: BTreeMap::new(),
            enabled: vec!["cyber.docker".into(), "cyber.vitals".into(), "cyber.cd".into()],
            apps: BTreeMap::new(),
            store_index: String::new(),
            store_key: String::new(),
            estado: Estado::Normal,
        }
    }
}

impl Config {
    /// Página onde o app está, se estiver em alguma.
    pub fn page_of(&self, id: &str) -> Option<usize> {
        self.pages.iter().position(|p| p.apps.iter().any(|a| a == id))
    }

    /// Página pedida por `--tab`: pelo nome da página ou por um app que está nela, com o id
    /// inteiro (`cyber.docker`) ou só a última parte (`docker`). Sem diferença de
    /// maiúsculas; o nome da página ganha de um app com o mesmo nome.
    pub fn find_page(&self, tab: &str) -> Option<usize> {
        let tab = tab.trim();
        let same = |s: &str| s.trim().eq_ignore_ascii_case(tab);
        self.pages.iter().position(|p| same(&p.name)).or_else(|| {
            self.pages.iter().position(|p| {
                p.apps.iter().any(|id| same(id) || id.rsplit_once('.').is_some_and(|(_, short)| same(short)))
            })
        })
    }

    /// Apps ligados da página, na ordem da página.
    pub fn page_apps(&self, page: usize) -> Vec<String> {
        self.pages.get(page).map_or_else(Vec::new, |p| p.apps.iter().filter(|a| self.is_enabled(a)).cloned().collect())
    }

    /// Retângulos dos apps ligados da página, na ordem de `page_apps`.
    pub fn rects(&self, page: usize) -> Vec<Rect> {
        self.page_apps(page).iter().map(|id| self.layout.get(id).copied().unwrap_or(Rect::new(0, 0, 1, 1))).collect()
    }

    /// Põe o app no fim da página `to` (tirando-o de onde estava). A posição é escolhida
    /// pelo próximo `arrange`: fica onde estava se couber, senão vai para a primeira vaga.
    pub fn move_to_page(&mut self, id: &str, to: usize) {
        if to >= self.pages.len() || self.page_of(id) == Some(to) {
            return;
        }
        for p in &mut self.pages {
            p.apps.retain(|a| a != id);
        }
        self.pages[to].apps.push(id.to_string());
    }

    /// Página nova no fim, com uma mascote que ainda não está em uso. Devolve o índice.
    pub fn add_page(&mut self) -> usize {
        let n = self.pages.len() + 1;
        let icon = crate::mascot::sprites::SPRITES
            .iter()
            .map(|s| s.key)
            .find(|k| !self.pages.iter().any(|p| p.icon == *k))
            .unwrap_or("neko");
        self.pages.push(Page { name: format!("Página {n}"), icon: icon.into(), ..Page::default() });
        self.pages.len() - 1
    }

    /// Apaga a página; os apps dela vão para a anterior (ou a primeira). A última não sai.
    pub fn remove_page(&mut self, i: usize) -> bool {
        if self.pages.len() <= 1 || i >= self.pages.len() {
            return false;
        }
        let page = self.pages.remove(i);
        let dest = i.saturating_sub(1).min(self.pages.len() - 1);
        self.pages[dest].apps.extend(page.apps);
        true
    }

    /// Garante que há ao menos uma página e que todo app ligado está em exatamente uma.
    fn ensure_pages(&mut self) {
        if self.pages.is_empty() {
            self.pages.push(Page::default());
        }
        let mut seen = std::collections::HashSet::new();
        for p in &mut self.pages {
            p.apps.retain(|a| seen.insert(a.clone()));
        }
        let orphans: Vec<String> = self.enabled.iter().filter(|id| !seen.contains(*id)).cloned().collect();
        self.pages[0].apps.extend(orphans);
    }

    /// Garante que todo app ligado tem página e lugar na grade, e que ninguém se sobrepõe.
    /// Cada um fica onde está se couber; senão vai para a primeira vaga e, sem vaga, a grade
    /// cresce. Devolve `true` se mudou algo.
    pub fn arrange(&mut self) -> bool {
        let before = (self.pages.clone(), self.layout.clone());
        self.ensure_pages();
        for i in 0..self.pages.len() {
            let ids = self.page_apps(i);
            let p = &mut self.pages[i];
            p.columns = p.columns.clamp(1, layout::MAX_COLS);
            p.rows = p.rows.clamp(1, layout::MAX_ROWS);
            let (cols, mut rows) = (p.columns, p.rows);
            let mut placed: Vec<Rect> = Vec::new();
            for id in &ids {
                // sem lugar guardado: uma célula, na primeira vaga
                let want = self.layout.get(id).copied().unwrap_or(Rect::new(cols, 0, 1, 1));
                let (r, used) = layout::add(&placed, want, cols, rows);
                rows = used;
                placed.push(r);
            }
            for (id, r) in ids.iter().zip(placed) {
                self.layout.insert(id.clone(), r);
            }
            self.pages[i].rows = rows;
        }
        before != (self.pages.clone(), self.layout.clone())
    }

    pub fn path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("miniutils/config.toml")
    }

    /// Lê a configuração; se não existir ou estiver quebrada, usa o padrão. Na primeira
    /// vez, copia a config da época em que o app se chamava CyberHub.
    pub fn load() -> Self {
        let path = Self::path();
        if !path.exists()
            && let Some(old) = dirs::config_dir().map(|d| d.join("cyberhub/config.toml")).filter(|p| p.exists()) {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match std::fs::copy(&old, &path) {
                    Ok(_) => log::info!("config copiada de {}", old.display()),
                    Err(e) => log::warn!("não consegui copiar {}: {e}", old.display()),
                }
            }
        let mut cfg = Self::load_from(&path);
        // deixa a linha `name` no arquivo, para o usuário achar onde trocar o nome
        let sem_nome = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| t.parse::<toml::Table>().ok())
            .is_some_and(|t| !t.contains_key("name"));
        if sem_nome && cfg.estado == Estado::Normal {
            cfg.save_to(&path);
        }
        cfg.publish_name();
        cfg
    }

    /// Torna o nome exibido visível para a bandeja e as notificações.
    pub fn publish_name(&self) {
        if let Ok(mut n) = DISPLAY_NAME.write() {
            *n = self.name.trim().to_string();
        }
    }

    fn load_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Config::default();
        };
        // Vazio não é uma config antiga (sem `version`), é uma gravação que não terminou:
        // migrá-lo guardaria um backup vazio e gravaria os padrões como se fossem escolha.
        if text.trim().is_empty() {
            return Config::default();
        }
        let mut tabela: toml::Table = match text.parse() {
            Ok(t) => t,
            Err(e) => return Self::quebrada(&e.to_string()),
        };
        let versao = match tabela.get("version") {
            None => 0,
            Some(v) => match v.as_integer().and_then(|i| u32::try_from(i).ok()) {
                Some(n) => n,
                None => return Self::quebrada("versão inválida"),
            },
        };

        // Arquivo de um MiniUtils mais novo: lê o que der, mas não mexe nele.
        if versao > VERSAO_ATUAL {
            log::warn!(
                "{} é da versão {versao}, mais nova que a {VERSAO_ATUAL} daqui; alterações não serão salvas",
                path.display()
            );
            let cfg = desserializa(tabela).unwrap_or_else(|e| {
                log::warn!("config inválida ({e}); usando o padrão");
                Config::default()
            });
            return Config { estado: Estado::SomenteLeitura, ..cfg };
        }

        let antiga = versao < VERSAO_ATUAL;
        if antiga {
            migra(&mut tabela, versao);
        }
        match desserializa(tabela) {
            Ok(mut cfg) => {
                // Grava a versão nova só depois de guardar o original.
                if antiga && backup(path) {
                    cfg.save_to(path);
                }
                cfg
            }
            Err(e) => Self::quebrada(&e),
        }
    }

    /// Configuração padrão para um arquivo que não pôde ser lido; o original é preservado.
    fn quebrada(motivo: &str) -> Self {
        log::warn!("config inválida ({motivo}); usando o padrão");
        Config { estado: Estado::Quebrado, ..Config::default() }
    }

    /// Grava a configuração. O texto é montado aqui e o disco fica com uma thread de
    /// gravação: o `fsync` não trava a interface (o modo de edição salva a cada tecla).
    /// Antes de sair, chame `flush_saves`.
    pub fn save(&mut self) {
        let path = Self::path();
        if let Some(text) = self.text_to_save(&path) {
            writer::submit(path, text);
        }
    }

    fn save_to(&mut self, path: &Path) {
        if let Some(text) = self.text_to_save(path) {
            write_logged(path, &text);
        }
    }

    /// O TOML a gravar em `path`, ou `None` se não deve gravar (somente leitura, ou o
    /// arquivo quebrado não pôde ser posto de lado).
    fn text_to_save(&mut self, path: &Path) -> Option<String> {
        match self.estado {
            Estado::SomenteLeitura => return None,
            Estado::Quebrado => {
                if path.exists() {
                    let destino = nome_livre(&com_sufixo(path, ".quebrado"));
                    if let Err(e) = std::fs::rename(path, &destino) {
                        log::warn!("não consegui mover {} para {}: {e}", path.display(), destino.display());
                        return None;
                    }
                }
                self.estado = Estado::Normal;
            }
            Estado::Normal => {}
        }
        match toml::to_string_pretty(self) {
            Ok(text) => Some(text),
            Err(e) => {
                log::error!("erro ao serializar config: {e}");
                None
            }
        }
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.enabled.iter().any(|e| e == id)
    }

    /// Valor de uma configuração do app, ou o padrão do manifesto.
    pub fn value(&self, m: &Manifest, key: &str) -> toml::Value {
        self.apps
            .get(&m.id)
            .and_then(|t| t.get(key))
            .cloned()
            .or_else(|| m.setting(key).map(|s| s.default.clone()))
            .unwrap_or(toml::Value::Boolean(false))
    }

    pub fn bool(&self, m: &Manifest, key: &str) -> bool {
        self.value(m, key).as_bool().unwrap_or(false)
    }

    pub fn int(&self, m: &Manifest, key: &str) -> i64 {
        self.value(m, key).as_integer().unwrap_or(0)
    }

    pub fn string(&self, m: &Manifest, key: &str) -> String {
        self.value(m, key).as_str().unwrap_or_default().to_string()
    }

    pub fn set(&mut self, app: &str, key: &str, value: toml::Value) {
        self.apps.entry(app.to_string()).or_default().insert(key.to_string(), value);
    }
}

/// Espera a thread de gravação terminar o que tem pendente (chamar antes de sair).
pub fn flush_saves() {
    writer::flush();
}

fn write_logged(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = write_atomic(path, text.as_bytes()) {
        log::error!("não consegui salvar {}: {e}", path.display());
    }
}

/// Thread única que grava a configuração. Guarda só o pedido mais novo: várias gravações
/// seguidas (setas no modo de edição) viram uma ida ao disco.
mod writer {
    use std::path::PathBuf;
    use std::sync::{Condvar, Mutex, Once};

    struct Queue {
        job: Option<(PathBuf, String)>,
        busy: bool,
    }

    static QUEUE: Mutex<Queue> = Mutex::new(Queue { job: None, busy: false });
    static CHANGED: Condvar = Condvar::new();
    static START: Once = Once::new();
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    pub fn submit(path: PathBuf, text: String) {
        START.call_once(|| {
            let ok = std::thread::Builder::new().name("miniutils-config".into()).spawn(run).is_ok();
            STARTED.store(ok, std::sync::atomic::Ordering::Relaxed);
        });
        if !STARTED.load(std::sync::atomic::Ordering::Relaxed) {
            // sem thread: grava aqui mesmo
            super::write_logged(&path, &text);
            return;
        }
        let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        q.job = Some((path, text));
        CHANGED.notify_all();
    }

    pub fn flush() {
        let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        while q.job.is_some() || q.busy {
            q = CHANGED.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn run() {
        loop {
            let (path, text) = {
                let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(job) = q.job.take() {
                        q.busy = true;
                        break job;
                    }
                    q = CHANGED.wait(q).unwrap_or_else(|e| e.into_inner());
                }
            };
            super::write_logged(&path, &text);
            let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            q.busy = false;
            CHANGED.notify_all();
        }
    }
}

/// Grava `bytes` em `path` sem nunca deixar o arquivo pela metade: escreve num temporário
/// ao lado, garante que foi para o disco e só então o põe no lugar (o `rename` é atômico).
/// Disco cheio, queda de energia ou o processo morto no meio deixam o arquivo anterior intacto.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = com_sufixo(path, ".tmp");
    let written = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// Desserializa a tabela TOML (já migrada) na configuração.
fn desserializa(tabela: toml::Table) -> Result<Config, String> {
    toml::Value::Table(tabela).try_into::<Config>().map_err(|e| e.to_string())
}

/// Leva a tabela da versão `de` até `VERSAO_ATUAL`, passo a passo.
fn migra(t: &mut toml::Table, de: u32) {
    for v in de..VERSAO_ATUAL {
        match v {
            0 => migra_0_para_1(t),
            1 => migra_1_para_2(t),
            2 => migra_2_para_3(t),
            3 => migra_3_para_4(t),
            _ => {}
        }
    }
    t.insert("version".to_string(), toml::Value::Integer(VERSAO_ATUAL.into()));
}

/// v0 -> v1: `theme` global vira tema de cada app (se ele não tiver o seu); `mascot` é descartado.
fn migra_0_para_1(t: &mut toml::Table) {
    let tema = t.remove("theme");
    t.remove("mascot");
    let Some(tema) = tema else { return };
    let apps = t.entry("apps").or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(apps) = apps.as_table_mut() else { return };
    for id in ["cyber.docker", "cyber.vitals"] {
        let app = apps.entry(id).or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(app) = app.as_table_mut()
            && !app.contains_key("theme") {
                app.insert("theme".to_string(), tema.clone());
            }
    }
}

/// v1 -> v2: a grade deixa de ser empacotada na ordem e cada app guarda a posição (`x`, `y`)
/// além do tamanho; a grade ganha um número de linhas. A posição vem do empacotamento antigo.
fn migra_1_para_2(t: &mut toml::Table) {
    let cols = t.get("columns").and_then(|v| v.as_integer()).unwrap_or(2).clamp(1, layout::MAX_COLS as i64) as u32;
    let enabled: Vec<String> = match t.get("enabled").and_then(|v| v.as_array()) {
        Some(a) => a.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        None => Config::default().enabled,
    };
    let mut lay = match t.remove("layout") {
        Some(toml::Value::Table(l)) => l,
        _ => toml::Table::new(),
    };
    let size = |lay: &toml::Table, id: &str, campo: &str, padrao: u32| -> u32 {
        lay.get(id).and_then(|v| v.get(campo)).and_then(|v| v.as_integer()).map_or(padrao, |n| n.max(1) as u32)
    };
    let spans: Vec<(u32, u32)> =
        enabled.iter().map(|id| (size(&lay, id, "w", cols).min(cols), size(&lay, id, "h", 1).min(3))).collect();
    let (placed, rows) = layout::pack(&spans, cols);
    for (id, r) in enabled.iter().zip(placed) {
        lay.insert(id.clone(), rect_toml(r));
    }
    // apps desligados guardam só o tamanho; a posição é escolhida ao religar
    let desligados: Vec<String> = lay.keys().filter(|k| !enabled.contains(k)).cloned().collect();
    for id in desligados {
        let r = Rect::new(0, 0, size(&lay, &id, "w", 1).min(cols), size(&lay, &id, "h", 1).min(3));
        lay.insert(id, rect_toml(r));
    }
    t.insert("layout".into(), toml::Value::Table(lay));
    t.insert("rows".into(), toml::Value::Integer(rows.clamp(1, layout::MAX_ROWS) as i64));
}

/// v2 -> v3: o mini-app CD Player entra ligado (na primeira vaga livre da grade).
fn migra_2_para_3(t: &mut toml::Table) {
    let Some(lista) = t.get_mut("enabled").and_then(|v| v.as_array_mut()) else { return };
    if !lista.iter().any(|v| v.as_str() == Some("cyber.cd")) {
        lista.push(toml::Value::String("cyber.cd".to_string()));
    }
}

/// v3 -> v4: a grade única vira a página "Geral" (barra lateral), com as colunas, as
/// linhas e os apps ligados de antes. As posições em `layout` continuam valendo.
fn migra_3_para_4(t: &mut toml::Table) {
    let mut page = toml::Table::new();
    page.insert("name".into(), toml::Value::String("Geral".into()));
    page.insert("icon".into(), toml::Value::String("neko".into()));
    page.insert("columns".into(), t.remove("columns").unwrap_or(toml::Value::Integer(2)));
    page.insert("rows".into(), t.remove("rows").unwrap_or(toml::Value::Integer(1)));
    let apps = t.get("enabled").cloned().unwrap_or_else(|| toml::Value::Array(Vec::new()));
    page.insert("apps".into(), apps);
    t.insert("pages".into(), toml::Value::Array(vec![toml::Value::Table(page)]));
}

fn rect_toml(r: Rect) -> toml::Value {
    let mut t = toml::Table::new();
    for (k, v) in [("x", r.x), ("y", r.y), ("w", r.w), ("h", r.h)] {
        t.insert(k.into(), toml::Value::Integer(v as i64));
    }
    toml::Value::Table(t)
}

/// Copia o arquivo para `.bak`, se ainda não existir. Devolve `false` se não conseguir.
fn backup(path: &Path) -> bool {
    let bak = com_sufixo(path, ".bak");
    if bak.exists() {
        return true;
    }
    match std::fs::copy(path, &bak) {
        Ok(_) => true,
        Err(e) => {
            log::warn!("não consegui criar {}: {e}", bak.display());
            false
        }
    }
}

/// Caminho com um sufixo extra, como `config.toml.bak`.
fn com_sufixo(path: &Path, sufixo: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(sufixo);
    PathBuf::from(s)
}

/// Primeiro nome livre: `base`, depois `base.1`, `base.2`…
fn nome_livre(base: &Path) -> PathBuf {
    let mut p = base.to_path_buf();
    let mut n = 1;
    while p.exists() {
        p = com_sufixo(base, &format!(".{n}"));
        n += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Diretório temporário exclusivo de cada teste; é apagado ao fim.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(nome: &str) -> Self {
            let d = std::env::temp_dir().join(format!("miniutils-teste-{}-{nome}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        }

        fn arquivo(&self) -> PathBuf {
            self.0.join("config.toml")
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn arquivo_vazio_nao_e_migrado_e_a_gravacao_nao_deixa_resto() {
        let tmp = Tmp::new("vazio");
        let path = tmp.arquivo();
        // uma gravação antiga interrompida deixou o arquivo vazio
        std::fs::write(&path, "  \n").unwrap();
        let mut cfg = Config::load_from(&path);
        assert!(cfg.estado == Estado::Normal);
        // nada de backup de um arquivo vazio, nem regravação "migrada"
        assert_eq!(std::fs::read_dir(&tmp.0).unwrap().count(), 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "  \n");

        cfg.name = "Meu hub".into();
        cfg.save_to(&path);
        // o temporário virou o arquivo; não sobra nada ao lado
        assert_eq!(std::fs::read_dir(&tmp.0).unwrap().count(), 1);
        assert_eq!(Config::load_from(&path).name, "Meu hub");

        // sem conseguir gravar (o destino é um diretório), o que existe fica como está
        let ocupado = tmp.0.join("ocupado");
        std::fs::create_dir_all(ocupado.join("dentro")).unwrap();
        assert!(write_atomic(&ocupado, b"x").is_err());
        assert!(ocupado.join("dentro").is_dir());
        assert!(!com_sufixo(&ocupado, ".tmp").exists());
    }

    const V0: &str = r#"
columns = 3
enabled = ["cyber.docker", "cyber.vitals"]
theme = "matrix"
mascot = "robo"

[apps."cyber.vitals"]
warn = 70
"#;

    #[test]
    fn v0_migra_theme_e_descarta_mascot() {
        let t = Tmp::new("v0");
        std::fs::write(t.arquivo(), V0).unwrap();

        let cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.version, VERSAO_ATUAL);
        assert_eq!(cfg.pages[0].columns, 3);
        assert_eq!(cfg.apps["cyber.docker"]["theme"].as_str(), Some("matrix"));
        assert_eq!(cfg.apps["cyber.vitals"]["theme"].as_str(), Some("matrix"));
        assert_eq!(cfg.apps["cyber.vitals"]["warn"].as_integer(), Some(70));

        // arquivo migrado gravado com a versão, sem `mascot`; original guardado no .bak
        let novo = std::fs::read_to_string(t.arquivo()).unwrap();
        assert!(novo.contains(&format!("version = {VERSAO_ATUAL}")));
        assert!(!novo.contains("mascot"));
        assert_eq!(std::fs::read_to_string(com_sufixo(&t.arquivo(), ".bak")).unwrap(), V0);
    }

    #[test]
    fn v0_nao_sobrescreve_theme_proprio_do_app() {
        let t = Tmp::new("v0-proprio");
        std::fs::write(
            t.arquivo(),
            "theme = \"matrix\"\n[apps.\"cyber.docker\"]\ntheme = \"neon\"\n",
        )
        .unwrap();

        let cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.apps["cyber.docker"]["theme"].as_str(), Some("neon"));
        assert_eq!(cfg.apps["cyber.vitals"]["theme"].as_str(), Some("matrix"));
    }

    #[test]
    fn v1_ganha_posicoes_do_empacotamento_antigo() {
        let t = Tmp::new("v1");
        std::fs::write(
            t.arquivo(),
            "version = 1\ncolumns = 3\nenabled = [\"cyber.docker\", \"cyber.vitals\"]\n\
             [layout.\"cyber.docker\"]\nw = 2\nh = 1\n[layout.\"cyber.vitals\"]\nw = 1\nh = 2\n\
             [layout.\"outro\"]\nw = 2\nh = 1\n",
        )
        .unwrap();

        let cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.version, VERSAO_ATUAL);
        assert_eq!(cfg.layout["cyber.docker"], Rect::new(0, 0, 2, 1));
        assert_eq!(cfg.layout["cyber.vitals"], Rect::new(2, 0, 1, 2));
        assert_eq!(cfg.layout["outro"], Rect::new(0, 0, 2, 1));
        assert_eq!(cfg.pages[0].rows, 2);
        assert!(com_sufixo(&t.arquivo(), ".bak").exists());
    }

    #[test]
    fn v2_liga_o_cd_player_uma_vez_so() {
        let t = Tmp::new("v2-cd");
        std::fs::write(t.arquivo(), "version = 2\ncolumns = 2\nenabled = [\"cyber.docker\"]\n").unwrap();

        let cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.version, VERSAO_ATUAL);
        assert_eq!(cfg.enabled, vec!["cyber.docker".to_string(), "cyber.cd".to_string()]);
        assert!(com_sufixo(&t.arquivo(), ".bak").exists());

        // já migrado: reler não duplica o app
        let cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.enabled.iter().filter(|e| *e == "cyber.cd").count(), 1);
    }

    #[test]
    fn arrange_acha_lugar_para_app_religado() {
        let page = Page { columns: 2, rows: 1, apps: vec!["a".into(), "b".into()], ..Page::default() };
        let mut cfg = Config { pages: vec![page], enabled: vec!["a".into()], ..Config::default() };
        cfg.layout.insert("a".into(), Rect::new(0, 0, 2, 1));
        cfg.layout.insert("b".into(), Rect::new(0, 0, 1, 1));
        cfg.enabled.push("b".into());
        assert!(cfg.arrange());
        assert_eq!(cfg.layout["a"], Rect::new(0, 0, 2, 1));
        assert_eq!(cfg.layout["b"], Rect::new(0, 1, 1, 1));
        assert_eq!(cfg.pages[0].rows, 2);
        assert!(!cfg.arrange());
    }

    #[test]
    fn v3_vira_pagina_geral() {
        let t = Tmp::new("v3");
        std::fs::write(
            t.arquivo(),
            "version = 3\ncolumns = 3\nrows = 2\nenabled = [\"cyber.docker\", \"cyber.cd\"]\n\
             [layout.\"cyber.docker\"]\nx = 1\ny = 1\nw = 2\nh = 1\n",
        )
        .unwrap();

        let cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.version, VERSAO_ATUAL);
        assert_eq!(cfg.pages.len(), 1);
        let p = &cfg.pages[0];
        assert_eq!((p.name.as_str(), p.columns, p.rows), ("Geral", 3, 2));
        assert_eq!(p.apps, vec!["cyber.docker".to_string(), "cyber.cd".to_string()]);
        assert_eq!(cfg.layout["cyber.docker"], Rect::new(1, 1, 2, 1));
        let novo = std::fs::read_to_string(t.arquivo()).unwrap();
        let tabela: toml::Table = novo.parse().unwrap();
        assert!(!tabela.contains_key("columns") && !tabela.contains_key("rows"));
    }

    #[test]
    fn app_ligado_sem_pagina_vai_para_a_primeira() {
        let mut cfg = Config { pages: vec![Page::default(), Page::default()], enabled: vec!["a".into()], ..Config::default() };
        cfg.arrange();
        assert_eq!(cfg.page_of("a"), Some(0));
        assert_eq!(cfg.page_apps(0), vec!["a".to_string()]);
    }

    #[test]
    fn find_page_pelo_nome_ou_pelo_app() {
        let page = |name: &str, apps: &[&str]| Page {
            name: name.into(),
            apps: apps.iter().map(|a| a.to_string()).collect(),
            ..Page::default()
        };
        let cfg = Config {
            pages: vec![page("Geral", &["cyber.vitals"]), page("IA", &["cyber.docker", "io.github.x.uptime"]), page("Docker", &[])],
            ..Config::default()
        };
        assert_eq!(cfg.find_page("geral"), Some(0));
        assert_eq!(cfg.find_page(" IA "), Some(1));
        assert_eq!(cfg.find_page("cyber.vitals"), Some(0));
        assert_eq!(cfg.find_page("Vitals"), Some(0));
        assert_eq!(cfg.find_page("uptime"), Some(1));
        // a página chamada "Docker" ganha do app cyber.docker
        assert_eq!(cfg.find_page("docker"), Some(2));
        assert_eq!(cfg.find_page("cyber"), None);
        assert_eq!(cfg.find_page("nada"), None);
    }

    #[test]
    fn mover_e_apagar_paginas() {
        let mut cfg = Config { pages: vec![Page::default()], enabled: vec!["a".into(), "b".into()], ..Config::default() };
        cfg.arrange();
        let p = cfg.add_page();
        assert_eq!(p, 1);
        assert_ne!(cfg.pages[1].icon, cfg.pages[0].icon);
        cfg.move_to_page("b", 1);
        cfg.arrange();
        assert_eq!(cfg.page_apps(0), vec!["a".to_string()]);
        assert_eq!(cfg.page_apps(1), vec!["b".to_string()]);
        // app movido fica onde estava, se couber na grade nova
        assert_eq!(cfg.rects(1), vec![Rect::new(1, 0, 1, 1)]);

        assert!(cfg.remove_page(1));
        assert_eq!(cfg.page_of("b"), Some(0));
        // a última página não sai
        assert!(!cfg.remove_page(0));
    }

    #[test]
    fn versao_atual_carrega_sem_migrar_nem_criar_backup() {
        let t = Tmp::new("atual");
        std::fs::write(t.arquivo(), format!("version = {VERSAO_ATUAL}\nskin = \"matrix\"\nenabled = []\n")).unwrap();

        let cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.skin, "matrix");
        assert!(cfg.enabled.is_empty());
        assert!(!com_sufixo(&t.arquivo(), ".bak").exists());
    }

    #[test]
    fn versao_futura_so_leitura() {
        let t = Tmp::new("futura");
        let original = format!("version = {}\nskin = \"matrix\"\nnovidade = true\n", VERSAO_ATUAL + 1);
        std::fs::write(t.arquivo(), &original).unwrap();

        let mut cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.skin, "matrix");
        cfg.skin = "neon".into();
        cfg.save_to(&t.arquivo());
        assert_eq!(std::fs::read_to_string(t.arquivo()).unwrap(), original);
        assert!(!com_sufixo(&t.arquivo(), ".bak").exists());
    }

    #[test]
    fn toml_quebrado_e_preservado_ao_salvar() {
        let t = Tmp::new("quebrado");
        let quebrado = "skin = [\n";
        std::fs::write(t.arquivo(), quebrado).unwrap();

        let mut cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.skin, Config::default().skin);
        // nada é gravado até o primeiro save
        assert_eq!(std::fs::read_to_string(t.arquivo()).unwrap(), quebrado);

        cfg.skin = "matrix".into();
        cfg.save_to(&t.arquivo());
        assert_eq!(std::fs::read_to_string(com_sufixo(&t.arquivo(), ".quebrado")).unwrap(), quebrado);
        let novo = Config::load_from(&t.arquivo());
        assert_eq!(novo.skin, "matrix");
        assert_eq!(novo.version, VERSAO_ATUAL);
    }

    #[test]
    fn save_sem_arquivo_cria_com_versao() {
        let t = Tmp::new("novo");
        let mut cfg = Config::load_from(&t.arquivo());
        cfg.save_to(&t.arquivo());
        let texto = std::fs::read_to_string(t.arquivo()).unwrap();
        assert!(texto.contains(&format!("version = {VERSAO_ATUAL}")));
        assert!(!com_sufixo(&t.arquivo(), ".quebrado").exists());
    }
}
