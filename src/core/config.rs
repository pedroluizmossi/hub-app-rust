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
pub const VERSAO_ATUAL: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Versão do formato do arquivo (ausente = 0, o formato antigo).
    pub version: u32,
    /// Nome exibido na janela, na bandeja e nas notificações (o usuário pode trocar).
    pub name: String,
    /// Colunas da grade do painel.
    pub columns: u32,
    /// Linhas da grade do painel.
    pub rows: u32,
    /// Visual da janela (ver `src/skin`).
    pub skin: String,
    /// Posição e tamanho de cada app na grade, em células: `[layout."cyber.docker"]`.
    pub layout: BTreeMap<String, Rect>,
    /// Tamanho da janela em pixels lógicos (lembrado entre aberturas).
    pub window: Option<(u32, u32)>,
    /// Mini-apps ligados, na ordem das abas.
    pub enabled: Vec<String>,
    /// Configurações de cada mini-app: `[apps."cyber.docker"]`.
    pub apps: BTreeMap<String, toml::Table>,
    /// Como salvar, conforme o arquivo lido (não vai para o TOML).
    #[serde(skip)]
    estado: Estado,
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
            columns: 2,
            rows: 1,
            skin: "neon".into(),
            window: None,
            layout: BTreeMap::new(),
            enabled: vec!["cyber.docker".into(), "cyber.vitals".into()],
            apps: BTreeMap::new(),
            estado: Estado::Normal,
        }
    }
}

impl Config {
    /// Retângulos dos apps ligados, na ordem de `enabled`.
    pub fn rects(&self) -> Vec<Rect> {
        self.enabled.iter().map(|id| self.layout.get(id).copied().unwrap_or(Rect::new(0, 0, 1, 1))).collect()
    }

    /// Garante que todo app ligado tem lugar na grade e que ninguém se sobrepõe. Cada um
    /// fica onde está se couber; senão vai para a primeira vaga e, sem vaga, a grade cresce.
    /// Devolve `true` se mudou algo.
    pub fn arrange(&mut self) -> bool {
        let before = (self.columns, self.rows, self.layout.clone());
        self.columns = self.columns.clamp(1, layout::MAX_COLS);
        self.rows = self.rows.clamp(1, layout::MAX_ROWS);
        let (cols, mut rows) = (self.columns, self.rows);

        let mut placed: Vec<Rect> = Vec::new();
        for id in &self.enabled {
            // sem lugar guardado: uma célula, na primeira vaga
            let want = self.layout.get(id).copied().unwrap_or(Rect::new(cols, 0, 1, 1));
            let (r, used) = layout::add(&placed, want, cols, rows);
            rows = used;
            placed.push(r);
        }
        for (id, r) in self.enabled.iter().zip(placed) {
            self.layout.insert(id.clone(), r);
        }
        self.rows = rows;
        before != (self.columns, self.rows, self.layout.clone())
    }

    pub fn path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("miniutils/config.toml")
    }

    /// Lê a configuração; se não existir ou estiver quebrada, usa o padrão. Na primeira
    /// vez, copia a config da época em que o app se chamava CyberHub.
    pub fn load() -> Self {
        let path = Self::path();
        if !path.exists() {
            if let Some(old) = dirs::config_dir().map(|d| d.join("cyberhub/config.toml")).filter(|p| p.exists()) {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                match std::fs::copy(&old, &path) {
                    Ok(_) => eprintln!("miniutils: config copiada de {}", old.display()),
                    Err(e) => eprintln!("miniutils: não consegui copiar {}: {e}", old.display()),
                }
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
            eprintln!(
                "miniutils: {} é da versão {versao}, mais nova que a {VERSAO_ATUAL} daqui; alterações não serão salvas",
                path.display()
            );
            let cfg = desserializa(tabela).unwrap_or_else(|e| {
                eprintln!("miniutils: config inválida ({e}); usando o padrão");
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
        eprintln!("miniutils: config inválida ({motivo}); usando o padrão");
        Config { estado: Estado::Quebrado, ..Config::default() }
    }

    pub fn save(&mut self) {
        let path = Self::path();
        self.save_to(&path);
    }

    fn save_to(&mut self, path: &Path) {
        match self.estado {
            Estado::SomenteLeitura => return,
            Estado::Quebrado => {
                if path.exists() {
                    let destino = nome_livre(&com_sufixo(path, ".quebrado"));
                    if let Err(e) = std::fs::rename(path, &destino) {
                        eprintln!("miniutils: não consegui mover {} para {}: {e}", path.display(), destino.display());
                        return;
                    }
                }
                self.estado = Estado::Normal;
            }
            Estado::Normal => {}
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match toml::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(path, text) {
                    eprintln!("miniutils: não consegui salvar {}: {e}", path.display());
                }
            }
            Err(e) => eprintln!("miniutils: erro ao serializar config: {e}"),
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
        if let Some(app) = app.as_table_mut() {
            if !app.contains_key("theme") {
                app.insert("theme".to_string(), tema.clone());
            }
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
            eprintln!("miniutils: não consegui criar {}: {e}", bak.display());
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
        assert_eq!(cfg.columns, 3);
        assert_eq!(cfg.apps["cyber.docker"]["theme"].as_str(), Some("matrix"));
        assert_eq!(cfg.apps["cyber.vitals"]["theme"].as_str(), Some("matrix"));
        assert_eq!(cfg.apps["cyber.vitals"]["warn"].as_integer(), Some(70));

        // arquivo migrado gravado com a versão, sem `mascot`; original guardado no .bak
        let novo = std::fs::read_to_string(t.arquivo()).unwrap();
        assert!(novo.contains("version = 2"));
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
        assert_eq!(cfg.rows, 2);
        assert!(com_sufixo(&t.arquivo(), ".bak").exists());
    }

    #[test]
    fn arrange_acha_lugar_para_app_religado() {
        let mut cfg = Config { columns: 2, rows: 1, enabled: vec!["a".into()], ..Config::default() };
        cfg.layout.insert("a".into(), Rect::new(0, 0, 2, 1));
        cfg.layout.insert("b".into(), Rect::new(0, 0, 1, 1));
        cfg.enabled.push("b".into());
        assert!(cfg.arrange());
        assert_eq!(cfg.layout["a"], Rect::new(0, 0, 2, 1));
        assert_eq!(cfg.layout["b"], Rect::new(0, 1, 1, 1));
        assert_eq!(cfg.rows, 2);
        assert!(!cfg.arrange());
    }

    #[test]
    fn v2_atual_carrega_sem_migrar_nem_criar_backup() {
        let t = Tmp::new("v2");
        std::fs::write(t.arquivo(), "version = 2\ncolumns = 4\nenabled = []\n").unwrap();

        let cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.columns, 4);
        assert!(cfg.enabled.is_empty());
        assert!(!com_sufixo(&t.arquivo(), ".bak").exists());
    }

    #[test]
    fn versao_futura_so_leitura() {
        let t = Tmp::new("futura");
        let original = "version = 3\ncolumns = 4\nnovidade = true\n";
        std::fs::write(t.arquivo(), original).unwrap();

        let mut cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.columns, 4);
        cfg.columns = 1;
        cfg.save_to(&t.arquivo());
        assert_eq!(std::fs::read_to_string(t.arquivo()).unwrap(), original);
        assert!(!com_sufixo(&t.arquivo(), ".bak").exists());
    }

    #[test]
    fn toml_quebrado_e_preservado_ao_salvar() {
        let t = Tmp::new("quebrado");
        let quebrado = "columns = [\n";
        std::fs::write(t.arquivo(), quebrado).unwrap();

        let mut cfg = Config::load_from(&t.arquivo());
        assert_eq!(cfg.columns, Config::default().columns);
        // nada é gravado até o primeiro save
        assert_eq!(std::fs::read_to_string(t.arquivo()).unwrap(), quebrado);

        cfg.columns = 3;
        cfg.save_to(&t.arquivo());
        assert_eq!(std::fs::read_to_string(com_sufixo(&t.arquivo(), ".quebrado")).unwrap(), quebrado);
        let novo = Config::load_from(&t.arquivo());
        assert_eq!(novo.columns, 3);
        assert_eq!(novo.version, VERSAO_ATUAL);
    }

    #[test]
    fn save_sem_arquivo_cria_com_versao() {
        let t = Tmp::new("novo");
        let mut cfg = Config::load_from(&t.arquivo());
        cfg.save_to(&t.arquivo());
        let texto = std::fs::read_to_string(t.arquivo()).unwrap();
        assert!(texto.contains("version = 2"));
        assert!(!com_sufixo(&t.arquivo(), ".quebrado").exists());
    }
}
