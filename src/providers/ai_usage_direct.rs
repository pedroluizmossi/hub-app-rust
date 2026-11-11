//! Leitura direta dos limites. Os formatos e endpoints seguem ai-usagebar
//! (MIT, Copyright 2026 AkitaOnRails); ver THIRD_PARTY_NOTICES.md.
//! Renova credenciais Claude quando necessário; não imprime erros brutos ou tokens.
use std::io::Read;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::redirect::Policy;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use super::{UsageCard, UsageWindow};

fn client() -> Result<&'static Client, String> {
    static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();
    CLIENT.get_or_init(|| Client::builder().timeout(Duration::from_secs(12)).redirect(Policy::none())
        .build().map_err(|_| "Cliente HTTPS indisponível".into())).as_ref().map_err(Clone::clone)
}

fn get_json(request: RequestBuilder) -> Result<Value, String> {
    let mut response = request.send().map_err(|_| "Falha de conexão".to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 | 403 => "Autenticação recusada".into(),
            429 => "Limite de consultas; tente mais tarde".into(),
            n => format!("Serviço indisponível (HTTP {n})"),
        });
    }
    let mut data = Vec::new();
    response.by_ref().take(1_048_577).read_to_end(&mut data).map_err(|_| "Resposta incompleta")?;
    if data.len() > 1_048_576 { return Err("Resposta muito grande".into()); }
    serde_json::from_slice(&data).map_err(|_| "Resposta de uso inválida".into())
}

fn object(path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|_| "Login local não encontrado")?;
    if bytes.len() > 1024 * 1024 { return Err("Arquivo de autenticação inválido".into()); }
    serde_json::from_slice(&bytes).map_err(|_| "Login local inválido".into())
}
fn nonempty(v: &Value) -> Option<&str> { v.as_str().filter(|s| !s.trim().is_empty() && !s.contains(['\r','\n'])) }
fn ts(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().filter(|f| f.is_finite()).map(|n| n as i64))
        .or_else(|| v.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp()))
}
fn epoch() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |t| t.as_secs() as i64) }
fn percent(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).filter(|v| v.is_finite() && (0.0..=101.0).contains(v))
}
fn meter(label: &str, used: f64, reset: Option<i64>) -> UsageWindow {
    UsageWindow { label: label.into(), used: used.clamp(0.0, 100.0), resets_at: reset, ..Default::default() }
}
fn card(name: &str, plan: String, windows: Vec<UsageWindow>) -> Result<UsageCard, String> {
    if windows.is_empty() { return Err("Sem limites disponíveis".into()); }
    Ok(UsageCard { name: name.into(), plan, windows, status: "Atualizado".into(), ..Default::default() })
}

pub(super) fn claude() -> Result<UsageCard, String> {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".claude"))).ok_or("Diretório Claude indisponível")?;
    let path = base.join(".credentials.json");
    let mut creds = object(&path)?;
    if creds["claudeAiOauth"]["expiresAt"].as_i64().is_some_and(|t| t / 1000 <= epoch() + 60)
        && nonempty(&creds["claudeAiOauth"]["refreshToken"]).is_some() {
        creds = refresh_claude(&path, "https://platform.claude.com/v1/oauth/token")?;
    }
    let oauth = &creds["claudeAiOauth"];
    let token = nonempty(&oauth["accessToken"]).ok_or("Claude Code sem login")?;
    if oauth["expiresAt"].as_i64().is_some_and(|t| t / 1000 <= epoch()) {
        return Err("Login Claude expirado; abra o Claude Code".into());
    }
    let data = get_json(client()?.get("https://api.anthropic.com/api/oauth/usage")
        .bearer_auth(token).header("anthropic-beta", "oauth-2025-04-20")
        .header("user-agent", "claude-cli/2.1.319 (external, cli)").header("accept", "application/json"))?;
    parse_claude(&data, oauth["subscriptionType"].as_str().unwrap_or(""))
}

/// Renova a credencial no mesmo arquivo, preservando todos os campos alheios.
/// A escrita temporária é preparada antes do pedido: um token rotacionado não
/// pode ser descartado silenciosamente. Nunca registra corpo nem credenciais.
fn refresh_claude(path: &Path, endpoint: &str) -> Result<Value, String> {
    let parent = path.parent().ok_or("Caminho Claude inválido")?;
    if std::fs::symlink_metadata(path).map_err(|_| "Login Claude indisponível")?.file_type().is_symlink() {
        return Err("Login Claude é um link; renove pelo Claude Code".into());
    }
    let lock_path = parent.join(".miniutils-refresh.lock");
    let lock = std::fs::OpenOptions::new().write(true).create(true).mode(0o600).open(lock_path).map_err(|_| "Bloqueio Claude indisponível")?;
    // SAFETY: o descritor pertence a `lock`, que permanece aberto até o fim da função.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("Outra atualização Claude em andamento".into());
    }
    let original = std::fs::read(path).map_err(|_| "Login Claude indisponível")?;
    let mut data: Value = serde_json::from_slice(&original).map_err(|_| "Login Claude inválido")?;
    let oauth = &data["claudeAiOauth"];
    if oauth["expiresAt"].as_i64().is_some_and(|t| t / 1000 > epoch() + 60) { return Ok(data); }
    let refresh = nonempty(&oauth["refreshToken"]).ok_or("Renove o login no Claude Code")?.to_string();
    let tmp = parent.join(format!(".miniutils-credentials-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)
        .map_err(|_| "Não foi possível preparar a renovação Claude")?;
    let result = (|| -> Result<Value, String> {
        let response = get_json(client()?.post(endpoint).header("anthropic-beta", "oauth-2025-04-20")
            .header("user-agent", "claude-cli/1.0")
            .json(&json!({"grant_type":"refresh_token","client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e","refresh_token":refresh})))?;
        let access = nonempty(&response["access_token"]).ok_or("Renovação Claude inválida")?;
        let duration = response["expires_in"].as_u64().filter(|v| *v > 0 && *v <= 31_536_000).ok_or("Validade Claude inválida")?;
        let updated = data["claudeAiOauth"].as_object_mut().ok_or("Login Claude inválido")?;
        updated.insert("accessToken".into(), json!(access));
        if let Some(rotated) = nonempty(&response["refresh_token"]) { updated.insert("refreshToken".into(), json!(rotated)); }
        updated.insert("expiresAt".into(), json!((epoch() + duration as i64) * 1000));
        serde_json::to_writer(&mut file, &data).map_err(|_| "Não foi possível salvar o login Claude renovado")?;
        file.write_all(b"\n").and_then(|_| file.sync_all()).map_err(|_| "Não foi possível salvar o login Claude renovado")?;
        if std::fs::read(path).map_err(|_| "Login Claude alterado durante renovação")? != original {
            return Err("Login Claude alterado durante renovação; confira o login".into());
        }
        std::fs::rename(&tmp, path).map_err(|_| "Não foi possível salvar o login Claude renovado")?;
        Ok(data)
    })();
    if result.is_err() { let _ = std::fs::remove_file(&tmp); }
    result
}

fn parse_claude(data: &Value, plan: &str) -> Result<UsageCard, String> {
    let mut windows = Vec::new();
    for (key, label) in [("five_hour", "5 horas"), ("seven_day", "Semanal"), ("seven_day_sonnet", "Semanal Sonnet")] {
        let v = &data[key];
        if let Some(p) = percent(&v["utilization"]) { windows.push(meter(label, p, ts(&v["resets_at"]))); }
    }
    if let Some(limits) = data["limits"].as_array() {
        for limit in limits.iter().filter(|x| x["kind"] == "weekly_scoped").take(10) {
            if let (Some(p), Some(model)) = (percent(&limit["percent"]), limit.pointer("/scope/model/display_name").and_then(Value::as_str)) {
                windows.push(meter(&format!("Semanal {}", model.chars().take(40).collect::<String>()), p, ts(&limit["resets_at"])));
            }
        }
    }
    card("Claude", plan.chars().take(40).collect(), windows)
}

pub(super) fn codex() -> Result<UsageCard, String> {
    let base = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|p| p.join(".codex"))).ok_or("Diretório Codex indisponível")?;
    let auth = object(&base.join("auth.json"))?;
    let token = nonempty(&auth["tokens"]["access_token"]).ok_or("Codex sem login ChatGPT")?;
    let mut request = client()?.get("https://chatgpt.com/backend-api/wham/usage").bearer_auth(token).header("user-agent", "codex-cli");
    if let Some(id) = nonempty(&auth["tokens"]["account_id"]) { request = request.header("ChatGPT-Account-Id", id); }
    parse_codex(&get_json(request)?)
}

fn parse_codex(data: &Value) -> Result<UsageCard, String> {
    let mut windows = Vec::new();
    for (key, fallback) in [("primary_window", "Limite principal"), ("secondary_window", "Limite secundário")] {
        let v = &data["rate_limit"][key];
        if let Some(p) = percent(&v["used_percent"]) {
            let label = match v["limit_window_seconds"].as_i64() { Some(18000) => "5 horas", Some(604800) => "Semanal", _ => fallback };
            windows.push(meter(label, p, ts(&v["reset_at"])));
        }
    }
    card("Codex", data["plan_type"].as_str().unwrap_or("").chars().take(40).collect(), windows)
}

fn database(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| "Banco de autenticação indisponível")?;
    conn.busy_timeout(Duration::from_millis(500)).map_err(|_| "Banco de autenticação indisponível")?;
    Ok(conn)
}
fn data_dir() -> Result<PathBuf, String> { dirs::data_dir().ok_or_else(|| "Diretório de dados indisponível".into()) }

struct OpenCodeAuth { token: String, org: Option<String> }

fn opencode_token() -> Result<OpenCodeAuth, String> {
    if let Ok(key) = std::env::var("OPENCODE_GO_API_KEY") {
        if !key.trim().is_empty() && !key.contains(['\r','\n']) { return Ok(OpenCodeAuth { token: key, org: None }); }
    }
    let dir = data_dir()?.join("opencode");
    // Na versão 2, o banco guarda a sessão ativa; um auth.json antigo pode
    // conter uma chave revogada de outra conta e não deve prevalecer.
    if let Ok(conn) = database(&dir.join("opencode.db")) {
        if let Ok(mut stmt) = conn.prepare("SELECT value FROM credential WHERE integration_id IN ('opencode-go','opencode') AND active = 1 ORDER BY CASE integration_id WHEN 'opencode-go' THEN 0 ELSE 1 END, time_updated DESC") {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                for row in rows.flatten() {
                    let Ok(v) = serde_json::from_str::<Value>(&row) else { continue };
                    let token = match v["type"].as_str() {
                        Some("key") => nonempty(&v["key"]),
                        Some("oauth") if v["expires"].as_i64().is_some_and(|e| e > epoch() * 1000) => nonempty(&v["access"]),
                        _ => None,
                    };
                    if let Some(token) = token {
                        let org = nonempty(&v["metadata"]["orgID"]).map(str::to_string);
                        return Ok(OpenCodeAuth { token: token.into(), org });
                    }
                }
            }
        }
    }
    if let Ok(v) = object(&dir.join("auth.json")) {
        for id in ["opencode-go", "opencode"] {
            if matches!(v[id]["type"].as_str(), Some("api" | "zen")) {
                if let Some(key) = nonempty(&v[id]["key"]) { return Ok(OpenCodeAuth { token: key.into(), org: None }); }
            }
        }
    }
    Err("Entre no OpenCode Go para renovar o login".into())
}

pub(super) fn opencode() -> Result<UsageCard, String> {
    let auth = opencode_token()?;
    let mut request = client()?.get("https://opencode.ai/zen/go/v1/usage").bearer_auth(auth.token)
        .header("accept", "application/json").header("user-agent", "miniutils/0.1");
    if let Some(org) = auth.org { request = request.header("x-opencode-org-id", org); }
    let data = get_json(request)?;
    parse_opencode(&data)
}
fn parse_opencode(data: &Value) -> Result<UsageCard, String> {
    let mut windows = Vec::new();
    for (key,label) in [("rolling","5 horas"),("weekly","Semanal"),("monthly","Mensal")] {
        let v = &data["usage"][key];
        if let Some(p) = percent(&v["percent"]) { windows.push(meter(label, p, ts(&v["resetsAt"]))); }
    }
    card("OpenCode Go", String::new(), windows)
}

pub(super) fn kiro() -> Result<UsageCard, String> {
    if std::env::var_os("KIRO_API_KEY").is_some_and(|key| !key.is_empty()) {
        return kiro_api_key();
    }
    let conn = match database(&data_dir()?.join("kiro-cli/data.sqlite3")) {
        Ok(conn) => conn,
        Err(_) => return kiro_api_key(),
    };
    let raw: String = match conn.query_row("SELECT value FROM auth_kv WHERE key = ?1", ["kirocli:odic:token"], |r| r.get(0)) {
        Ok(value) => value,
        Err(_) => return kiro_api_key(),
    };
    let token: Value = serde_json::from_str(&raw).map_err(|_| "Login Kiro inválido")?;
    let access = nonempty(&token["access_token"]).ok_or("Login Kiro inválido")?;
    if ts(&token["expires_at"]).is_some_and(|t| t <= epoch()) { return Err("Login Kiro expirado".into()); }
    let region = nonempty(&token["region"]).filter(|s| s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')).ok_or("Região Kiro inválida")?;
    let profile: Option<String> = conn.query_row("SELECT value FROM state WHERE key = ?1", ["api.codewhisperer.profile"], |r| r.get(0)).ok();
    let profile = profile.and_then(|p| serde_json::from_str::<Value>(&p).ok()).and_then(|p| p["arn"].as_str().map(str::to_string)).unwrap_or_default();
    let data = get_json(client()?.post(format!("https://codewhisperer.{region}.amazonaws.com/"))
        .bearer_auth(access).header("Content-Type", "application/x-amz-json-1.0")
        .header("X-Amz-Target", "AmazonCodeWhispererService.GetUsageLimits")
        .json(&json!({"origin":"AI_EDITOR","profileArn":profile,"resourceType":"AGENTIC_REQUEST"})))?;
    parse_kiro(&data)
}

fn kiro_api_key() -> Result<UsageCard, String> {
    let token = std::env::var("KIRO_API_KEY").map_err(|_| "Login Kiro indisponível")?;
    if token.trim().is_empty() || token.contains(['\r', '\n']) { return Err("Chave Kiro inválida".into()); }
    let region = std::env::var("AWS_REGION").or_else(|_| std::env::var("AWS_DEFAULT_REGION")).unwrap_or_else(|_| "us-east-1".into());
    if region.len() > 32 || !region.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err("Região Kiro inválida".into());
    }
    let data = get_json(client()?.post(format!("https://codewhisperer.{region}.amazonaws.com/"))
        .bearer_auth(token).header("tokentype", "API_KEY").header("Content-Type", "application/x-amz-json-1.0")
        .header("X-Amz-Target", "AmazonCodeWhispererService.GetUsageLimits")
        .json(&json!({"origin":"AI_EDITOR","resourceType":"AGENTIC_REQUEST"})))?;
    parse_kiro(&data)
}
fn parse_kiro(data: &Value) -> Result<UsageCard, String> {
    let list = data["usageBreakdownList"].as_array().ok_or("Créditos Kiro indisponíveis")?;
    let item = list.iter().find(|v| v["resourceType"] == "CREDIT")
        .or_else(|| (list.len() == 1 && list[0]["resourceType"].is_null()).then(|| &list[0])).ok_or("Créditos Kiro indisponíveis")?;
    let used = item["currentUsageWithPrecision"].as_f64().filter(|v| v.is_finite() && *v >= 0.0).ok_or("Uso Kiro inválido")?;
    let total = item["usageLimitWithPrecision"].as_f64().filter(|v| v.is_finite() && *v > 0.0).ok_or("Limite Kiro inválido")?;
    let mut w = meter("Créditos", used / total * 100.0, ts(&data["nextDateReset"]));
    w.detail = format!("{used:.2} / {total:.0} créditos");
    card("Kiro", data["subscriptionInfo"]["subscriptionTitle"].as_str().unwrap_or("").chars().take(40).collect(), vec![w])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    #[test]
    fn claude_limites_ausentes_nao_viram_zero() {
        let c = parse_claude(&json!({"five_hour":{"utilization":23.5,"resets_at":"2026-12-01T00:00:00Z"},"seven_day":null}), "pro").unwrap();
        assert_eq!(c.windows.len(), 1); assert_eq!(c.windows[0].used, 23.5); assert!(c.windows[0].resets_at.is_some());
        assert!(parse_claude(&json!({}), "").is_err());
    }
    #[test]
    fn opencode_janelas_e_validacao() {
        let c = parse_opencode(&json!({"usage":{"rolling":{"percent":10,"resetsAt":"2026-12-01T00:00:00Z"},"weekly":{"percent":101.0}}})).unwrap();
        assert_eq!(c.windows.len(), 2); assert_eq!(c.windows[1].used, 100.0);
        assert!(parse_opencode(&json!({"usage":{"rolling":{"percent":-1}}})).is_err());
    }
    #[test]
    fn kiro_creditos_e_pool() {
        let c = parse_kiro(&json!({"subscriptionInfo":{"subscriptionTitle":"PRO+"},"usageBreakdownList":[{"resourceType":"CREDIT","currentUsageWithPrecision":125.5,"usageLimitWithPrecision":2000.0}],"nextDateReset":1796083200})).unwrap();
        assert_eq!(c.windows[0].detail, "125.50 / 2000 créditos");
        assert_eq!(c.windows[0].resets_at, Some(1796083200));
    }
    #[test]
    fn codex_identifica_janela_pela_duracao() {
        let c = parse_codex(&json!({"rate_limit":{"primary_window":{"used_percent":12,"limit_window_seconds":604800}}})).unwrap();
        assert_eq!(c.windows[0].label, "Semanal");
    }

    #[test]
    fn renovacao_claude_preserva_outros_campos_e_token_rotacionado() {
        let dir = std::env::temp_dir().join(format!("miniutils-refresh-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.json");
        std::fs::write(&path, json!({"claudeAiOauth":{"accessToken":"old","refreshToken":"old-refresh","expiresAt":1},"other":{"keep":true}}).to_string()).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).unwrap();
            let body = r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let result = refresh_claude(&path, &endpoint).unwrap();
        server.join().unwrap();
        assert_eq!(result["claudeAiOauth"]["refreshToken"], "rotated");
        assert_eq!(result["other"]["keep"], true);
        assert_eq!(object(&path).unwrap(), result);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(dir.join(".miniutils-refresh.lock")).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    #[ignore = "consulta serviços usando logins locais"]
    fn integracoes_diretas_locais() {
        for (name, result) in [("Claude", claude()), ("Codex", codex()), ("Kiro", kiro()), ("OpenCode Go", opencode())] {
            let card = result.unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!card.windows.is_empty());
            eprintln!("{name}: {} janela(s)", card.windows.len());
        }
    }
}
