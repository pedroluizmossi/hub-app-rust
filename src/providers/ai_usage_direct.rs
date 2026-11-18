//! Direct reading of the limits. Formats and endpoints follow ai-usagebar
//! (MIT, Copyright 2026 AkitaOnRails); see THIRD_PARTY_NOTICES.md.
//! Refreshes Claude credentials when needed; never prints raw errors or tokens.
use std::fmt;
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::redirect::Policy;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use super::{UsageCard, UsageWindow};
use crate::core::os::owner_only;
use crate::tr;

/// Why a reading failed: the text shown on the card and what kind of failure it is.
#[derive(Debug)]
pub(super) struct Failure {
    pub msg: String,
    pub kind: FailureKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FailureKind {
    Other,
    /// Login missing, expired or refused: it may mean the account changed.
    Auth,
    /// The service is throttling queries: back off before asking again.
    RateLimited,
}

impl From<&str> for Failure {
    fn from(msg: &str) -> Self {
        Failure { msg: msg.into(), kind: FailureKind::Other }
    }
}

impl From<String> for Failure {
    fn from(msg: String) -> Self {
        Failure { msg, kind: FailureKind::Other }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

/// A login problem (see [`FailureKind::Auth`]).
fn auth(msg: &str) -> Failure {
    Failure { msg: msg.into(), kind: FailureKind::Auth }
}

fn client() -> Result<&'static Client, Failure> {
    static CLIENT: OnceLock<Option<Client>> = OnceLock::new();
    CLIENT.get_or_init(|| Client::builder().timeout(Duration::from_secs(12)).redirect(Policy::none()).build().ok())
        .as_ref().ok_or_else(|| tr!("HTTPS client unavailable").into())
}

fn get_json(request: RequestBuilder) -> Result<Value, Failure> {
    let mut response = request.send().map_err(|_| tr!("Connection failed"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(match status.as_u16() {
            401 | 403 => auth(tr!("Authentication refused")),
            429 => Failure { msg: tr!("Too many requests; try again later").into(), kind: FailureKind::RateLimited },
            n => tr!("Service unavailable (HTTP {})", n).into(),
        });
    }
    let mut data = Vec::new();
    response.by_ref().take(1_048_577).read_to_end(&mut data).map_err(|_| tr!("Incomplete response"))?;
    if data.len() > 1_048_576 { return Err(tr!("Response too large").into()); }
    serde_json::from_slice(&data).map_err(|_| tr!("Invalid usage response").into())
}

fn object(path: &Path) -> Result<Value, Failure> {
    let bytes = std::fs::read(path).map_err(|_| auth(tr!("Local login not found")))?;
    if bytes.len() > 1024 * 1024 { return Err(tr!("Invalid authentication file").into()); }
    serde_json::from_slice(&bytes).map_err(|_| auth(tr!("Invalid local login")))
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
fn card(name: &str, plan: String, windows: Vec<UsageWindow>) -> Result<UsageCard, Failure> {
    if windows.is_empty() { return Err(tr!("No limits available").into()); }
    Ok(UsageCard { name: name.into(), plan, windows, status: tr!("Updated").into(), ..Default::default() })
}

pub(super) fn claude() -> Result<UsageCard, Failure> {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".claude"))).ok_or(tr!("Claude folder unavailable"))?;
    let path = base.join(".credentials.json");
    let mut creds = object(&path)?;
    if creds["claudeAiOauth"]["expiresAt"].as_i64().is_some_and(|t| t / 1000 <= epoch() + 60)
        && nonempty(&creds["claudeAiOauth"]["refreshToken"]).is_some() {
        creds = refresh_claude(&path, "https://platform.claude.com/v1/oauth/token")?;
    }
    let oauth = &creds["claudeAiOauth"];
    let token = nonempty(&oauth["accessToken"]).ok_or_else(|| auth(tr!("Claude Code is not signed in")))?;
    if oauth["expiresAt"].as_i64().is_some_and(|t| t / 1000 <= epoch()) {
        return Err(auth(tr!("Claude login expired; open Claude Code")));
    }
    let data = get_json(client()?.get("https://api.anthropic.com/api/oauth/usage")
        .bearer_auth(token).header("anthropic-beta", "oauth-2025-04-20")
        .header("user-agent", "claude-cli/2.1.319 (external, cli)").header("accept", "application/json"))?;
    parse_claude(&data, oauth["subscriptionType"].as_str().unwrap_or(""))
}

/// Rotated Claude login that is not in `.credentials.json` yet (the save failed):
/// `{"replaces": <refresh token it replaced>, "login": <credentials>}`. Owner-only, next to
/// the credentials, and only touched under the refresh lock.
const CLAUDE_PENDING: &str = ".miniutils-credentials.pending";

/// Refreshes the credential in the same file, keeping every unrelated field.
/// The temporary file is prepared before the request: a rotated token must not be
/// silently discarded. Once the service answered, the new login is first kept in
/// [`CLAUDE_PENDING`]; if saving it to the credentials fails (e.g. Claude Code holds the file
/// open on Windows), the next call retries before asking for another refresh. Never logs the
/// body or the credentials.
fn refresh_claude(path: &Path, endpoint: &str) -> Result<Value, Failure> {
    let parent = path.parent().ok_or(tr!("Invalid Claude path"))?;
    if std::fs::symlink_metadata(path).map_err(|_| auth(tr!("Claude login unavailable")))?.file_type().is_symlink() {
        return Err(auth(tr!("Claude login is a link; renew it in Claude Code")));
    }
    let lock_path = parent.join(".miniutils-refresh.lock");
    let lock = owner_only(std::fs::OpenOptions::new().write(true).create(true)).open(lock_path).map_err(|_| tr!("Claude lock unavailable"))?;
    if lock.try_lock().is_err() {
        return Err(tr!("Another Claude refresh in progress").into());
    }
    // the lock holds until the end of the function; it is released explicitly because a child
    // process spawned meanwhile by another thread shares the descriptor until its `exec`, and
    // closing ours alone would leave the lock held for that moment
    let _unlock = Unlock(lock);
    let original = std::fs::read(path).map_err(|_| auth(tr!("Claude login unavailable")))?;
    let mut data: Value = serde_json::from_slice(&original).map_err(|_| auth(tr!("Invalid Claude login")))?;
    let pending = parent.join(CLAUDE_PENDING);
    if let Some(login) = pending_claude(&pending, &data) {
        data = login;
        commit_claude(path, &pending, &data, &original);
    }
    let oauth = &data["claudeAiOauth"];
    if oauth["expiresAt"].as_i64().is_some_and(|t| t / 1000 > epoch() + 60) { return Ok(data); }
    let refresh = nonempty(&oauth["refreshToken"]).ok_or_else(|| auth(tr!("Renew the login in Claude Code")))?.to_string();
    let tmp = parent.join(format!(".miniutils-credentials-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut file = owner_only(std::fs::OpenOptions::new().write(true).create_new(true)).open(&tmp)
        .map_err(|_| tr!("Could not prepare the Claude refresh"))?;
    let requested = (|| -> Result<(), Failure> {
        let response = get_json(client()?.post(endpoint).header("anthropic-beta", "oauth-2025-04-20")
            .header("user-agent", "claude-cli/1.0")
            .json(&json!({"grant_type":"refresh_token","client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e","refresh_token":refresh})))?;
        let access = nonempty(&response["access_token"]).ok_or(tr!("Invalid Claude refresh"))?;
        let duration = response["expires_in"].as_u64().filter(|v| *v > 0 && *v <= 31_536_000).ok_or(tr!("Invalid Claude expiry"))?;
        let updated = data["claudeAiOauth"].as_object_mut().ok_or_else(|| auth(tr!("Invalid Claude login")))?;
        updated.insert("accessToken".into(), json!(access));
        if let Some(rotated) = nonempty(&response["refresh_token"]) { updated.insert("refreshToken".into(), json!(rotated)); }
        updated.insert("expiresAt".into(), json!((epoch() + duration as i64) * 1000));
        Ok(())
    })();
    if let Err(e) = requested {
        drop(file);
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // From here the old refresh token may already be invalid: whatever fails, the new login
    // is used for this reading and must not be deleted.
    let staged = write_json(&mut file, &json!({"replaces": refresh, "login": &data}))
        .and_then(|_| { drop(file); std::fs::rename(&tmp, &pending) });
    match staged {
        Ok(()) => commit_claude(path, &pending, &data, &original),
        Err(e) => log::warn!("ai_usage: couldn't keep the refreshed Claude login in {}: {e}", tmp.display()),
    }
    Ok(data)
}

/// Releases a file lock when dropped (see [`refresh_claude`]).
struct Unlock(std::fs::File);

impl Drop for Unlock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The login kept by a refresh whose save failed, merged into the `current` credentials.
/// `None` (and the pending file is removed) when there is none, it is unreadable, or the
/// credentials no longer hold the refresh token it replaced: someone else (Claude Code, a
/// new login) wrote a newer login since.
fn pending_claude(pending: &Path, current: &Value) -> Option<Value> {
    let bytes = std::fs::read(pending).ok()?;
    let kept: Option<Value> = serde_json::from_slice(&bytes).ok();
    let still_current = kept.as_ref().is_some_and(|k| {
        nonempty(&k["replaces"]).is_some() && k["replaces"] == current["claudeAiOauth"]["refreshToken"]
    });
    if !still_current {
        log::info!("ai_usage: discarding the kept Claude login; the credentials changed since");
        let _ = std::fs::remove_file(pending);
        return None;
    }
    let kept = kept?;
    let mut merged = current.clone();
    let oauth = merged["claudeAiOauth"].as_object_mut()?;
    for key in ["accessToken", "refreshToken", "expiresAt"] {
        let value = &kept["login"]["claudeAiOauth"][key];
        if !value.is_null() { oauth.insert(key.into(), value.clone()); }
    }
    Some(merged)
}

/// Saves `data` over the credentials if they still hold `original`, then drops the pending
/// copy. On failure the pending copy stays for the next try; only its path is logged.
fn commit_claude(path: &Path, pending: &Path, data: &Value, original: &[u8]) {
    let parent = path.parent().unwrap_or(Path::new("."));
    let tmp = parent.join(format!(".miniutils-credentials-{}", std::process::id()));
    let saved = (|| -> std::io::Result<()> {
        if std::fs::read(path)? != original {
            return Err(std::io::Error::other("the credentials changed during the refresh"));
        }
        let _ = std::fs::remove_file(&tmp);
        let mut file = owner_only(std::fs::OpenOptions::new().write(true).create_new(true)).open(&tmp)?;
        write_json(&mut file, data)?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    match saved {
        Ok(()) => { let _ = std::fs::remove_file(pending); }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            log::warn!("ai_usage: couldn't save the refreshed Claude login ({e}); kept in {} for the next try", pending.display());
        }
    }
}

fn write_json(file: &mut std::fs::File, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()
}

fn parse_claude(data: &Value, plan: &str) -> Result<UsageCard, Failure> {
    let mut windows = Vec::new();
    for (key, label) in [("five_hour", tr!("5 hours")), ("seven_day", tr!("Weekly")), ("seven_day_sonnet", tr!("Weekly Sonnet"))] {
        let v = &data[key];
        if let Some(p) = percent(&v["utilization"]) { windows.push(meter(label, p, ts(&v["resets_at"]))); }
    }
    if let Some(limits) = data["limits"].as_array() {
        for limit in limits.iter().filter(|x| x["kind"] == "weekly_scoped").take(10) {
            if let (Some(p), Some(model)) = (percent(&limit["percent"]), limit.pointer("/scope/model/display_name").and_then(Value::as_str)) {
                windows.push(meter(&tr!("Weekly {}", model.chars().take(40).collect::<String>()), p, ts(&limit["resets_at"])));
            }
        }
    }
    card("Claude", plan.chars().take(40).collect(), windows)
}

pub(super) fn codex() -> Result<UsageCard, Failure> {
    let base = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|p| p.join(".codex"))).ok_or(tr!("Codex folder unavailable"))?;
    let auth_file = object(&base.join("auth.json"))?;
    let token = nonempty(&auth_file["tokens"]["access_token"]).ok_or_else(|| auth(tr!("Codex is not signed in to ChatGPT")))?;
    let mut request = client()?.get("https://chatgpt.com/backend-api/wham/usage").bearer_auth(token).header("user-agent", "codex-cli");
    if let Some(id) = nonempty(&auth_file["tokens"]["account_id"]) { request = request.header("ChatGPT-Account-Id", id); }
    parse_codex(&get_json(request)?)
}

fn parse_codex(data: &Value) -> Result<UsageCard, Failure> {
    let mut windows = Vec::new();
    for (key, fallback) in [("primary_window", tr!("Main limit")), ("secondary_window", tr!("Secondary limit"))] {
        let v = &data["rate_limit"][key];
        if let Some(p) = percent(&v["used_percent"]) {
            let label = match v["limit_window_seconds"].as_i64() { Some(18000) => tr!("5 hours"), Some(604800) => tr!("Weekly"), _ => fallback };
            windows.push(meter(label, p, ts(&v["reset_at"])));
        }
    }
    card("Codex", data["plan_type"].as_str().unwrap_or("").chars().take(40).collect(), windows)
}

fn database(path: &Path) -> Result<Connection, Failure> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|_| tr!("Authentication database unavailable"))?;
    conn.busy_timeout(Duration::from_millis(500)).map_err(|_| tr!("Authentication database unavailable"))?;
    Ok(conn)
}
fn data_dir() -> Result<PathBuf, Failure> { crate::core::os::xdg_data_dir().ok_or_else(|| tr!("Data folder unavailable").into()) }

struct OpenCodeAuth { token: String, org: Option<String> }

fn opencode_token() -> Result<OpenCodeAuth, Failure> {
    if let Ok(key) = std::env::var("OPENCODE_GO_API_KEY")
        && !key.trim().is_empty() && !key.contains(['\r','\n']) { return Ok(OpenCodeAuth { token: key, org: None }); }
    let dir = data_dir()?.join("opencode");
    // In version 2 the database holds the active session; an old auth.json may hold a
    // revoked key from another account and must not win.
    if let Ok(conn) = database(&dir.join("opencode.db"))
        && let Ok(mut stmt) = conn.prepare("SELECT value FROM credential WHERE integration_id IN ('opencode-go','opencode') AND active = 1 ORDER BY CASE integration_id WHEN 'opencode-go' THEN 0 ELSE 1 END, time_updated DESC")
            && let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
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
    if let Ok(v) = object(&dir.join("auth.json")) {
        for id in ["opencode-go", "opencode"] {
            if matches!(v[id]["type"].as_str(), Some("api" | "zen"))
                && let Some(key) = nonempty(&v[id]["key"]) { return Ok(OpenCodeAuth { token: key.into(), org: None }); }
        }
    }
    Err(auth(tr!("Sign in to OpenCode Go to renew the login")))
}

pub(super) fn opencode() -> Result<UsageCard, Failure> {
    let auth = opencode_token()?;
    let mut request = client()?.get("https://opencode.ai/zen/go/v1/usage").bearer_auth(auth.token)
        .header("accept", "application/json").header("user-agent", "miniutils/0.1");
    if let Some(org) = auth.org { request = request.header("x-opencode-org-id", org); }
    let data = get_json(request)?;
    parse_opencode(&data)
}
fn parse_opencode(data: &Value) -> Result<UsageCard, Failure> {
    let mut windows = Vec::new();
    for (key,label) in [("rolling",tr!("5 hours")),("weekly",tr!("Weekly")),("monthly",tr!("Monthly"))] {
        let v = &data["usage"][key];
        if let Some(p) = percent(&v["percent"]) { windows.push(meter(label, p, ts(&v["resetsAt"]))); }
    }
    card("OpenCode Go", String::new(), windows)
}

pub(super) fn kiro() -> Result<UsageCard, Failure> {
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
    let token: Value = serde_json::from_str(&raw).map_err(|_| auth(tr!("Invalid Kiro login")))?;
    let access = nonempty(&token["access_token"]).ok_or_else(|| auth(tr!("Invalid Kiro login")))?;
    if ts(&token["expires_at"]).is_some_and(|t| t <= epoch()) { return Err(auth(tr!("Kiro login expired"))); }
    let region = nonempty(&token["region"]).filter(|s| s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')).ok_or(tr!("Invalid Kiro region"))?;
    let profile: Option<String> = conn.query_row("SELECT value FROM state WHERE key = ?1", ["api.codewhisperer.profile"], |r| r.get(0)).ok();
    let profile = profile.and_then(|p| serde_json::from_str::<Value>(&p).ok()).and_then(|p| p["arn"].as_str().map(str::to_string)).unwrap_or_default();
    let data = get_json(client()?.post(format!("https://codewhisperer.{region}.amazonaws.com/"))
        .bearer_auth(access).header("Content-Type", "application/x-amz-json-1.0")
        .header("X-Amz-Target", "AmazonCodeWhispererService.GetUsageLimits")
        .json(&json!({"origin":"AI_EDITOR","profileArn":profile,"resourceType":"AGENTIC_REQUEST"})))?;
    parse_kiro(&data)
}

fn kiro_api_key() -> Result<UsageCard, Failure> {
    let token = std::env::var("KIRO_API_KEY").map_err(|_| auth(tr!("Kiro login unavailable")))?;
    if token.trim().is_empty() || token.contains(['\r', '\n']) { return Err(tr!("Invalid Kiro key").into()); }
    let region = std::env::var("AWS_REGION").or_else(|_| std::env::var("AWS_DEFAULT_REGION")).unwrap_or_else(|_| "us-east-1".into());
    if region.len() > 32 || !region.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        return Err(tr!("Invalid Kiro region").into());
    }
    let data = get_json(client()?.post(format!("https://codewhisperer.{region}.amazonaws.com/"))
        .bearer_auth(token).header("tokentype", "API_KEY").header("Content-Type", "application/x-amz-json-1.0")
        .header("X-Amz-Target", "AmazonCodeWhispererService.GetUsageLimits")
        .json(&json!({"origin":"AI_EDITOR","resourceType":"AGENTIC_REQUEST"})))?;
    parse_kiro(&data)
}
fn parse_kiro(data: &Value) -> Result<UsageCard, Failure> {
    let list = data["usageBreakdownList"].as_array().ok_or(tr!("Kiro credits unavailable"))?;
    let item = list.iter().find(|v| v["resourceType"] == "CREDIT")
        .or_else(|| (list.len() == 1 && list[0]["resourceType"].is_null()).then(|| &list[0])).ok_or(tr!("Kiro credits unavailable"))?;
    let used = item["currentUsageWithPrecision"].as_f64().filter(|v| v.is_finite() && *v >= 0.0).ok_or(tr!("Invalid Kiro usage"))?;
    let total = item["usageLimitWithPrecision"].as_f64().filter(|v| v.is_finite() && *v > 0.0).ok_or(tr!("Invalid Kiro limit"))?;
    let mut w = meter(tr!("Credits"), used / total * 100.0, ts(&data["nextDateReset"]));
    w.detail = tr!("{} / {} credits", format!("{used:.2}"), format!("{total:.0}"));
    card("Kiro", data["subscriptionInfo"]["subscriptionTitle"].as_str().unwrap_or("").chars().take(40).collect(), vec![w])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    #[test]
    fn missing_claude_limits_do_not_become_zero() {
        let c = parse_claude(&json!({"five_hour":{"utilization":23.5,"resets_at":"2026-12-01T00:00:00Z"},"seven_day":null}), "pro").unwrap();
        assert_eq!(c.windows.len(), 1); assert_eq!(c.windows[0].used, 23.5); assert!(c.windows[0].resets_at.is_some());
        assert!(parse_claude(&json!({}), "").is_err());
    }
    #[test]
    fn opencode_windows_and_validation() {
        let c = parse_opencode(&json!({"usage":{"rolling":{"percent":10,"resetsAt":"2026-12-01T00:00:00Z"},"weekly":{"percent":101.0}}})).unwrap();
        assert_eq!(c.windows.len(), 2); assert_eq!(c.windows[1].used, 100.0);
        assert!(parse_opencode(&json!({"usage":{"rolling":{"percent":-1}}})).is_err());
    }
    #[test]
    fn kiro_credits_and_pool() {
        let c = parse_kiro(&json!({"subscriptionInfo":{"subscriptionTitle":"PRO+"},"usageBreakdownList":[{"resourceType":"CREDIT","currentUsageWithPrecision":125.5,"usageLimitWithPrecision":2000.0}],"nextDateReset":1796083200})).unwrap();
        assert_eq!(c.windows[0].detail, "125.50 / 2000 credits");
        assert_eq!(c.windows[0].resets_at, Some(1796083200));
    }
    #[test]
    fn codex_identifies_window_by_duration() {
        let c = parse_codex(&json!({"rate_limit":{"primary_window":{"used_percent":12,"limit_window_seconds":604800}}})).unwrap();
        assert_eq!(c.windows[0].label, "Weekly");
    }

    #[test]
    fn claude_refresh_keeps_other_fields_and_rotated_token() {
        let dir = std::env::temp_dir().join(format!("miniutils-refresh-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.json");
        std::fs::write(&path, json!({"claudeAiOauth":{"accessToken":"old","refreshToken":"old-refresh","expiresAt":1},"other":{"keep":true}}).to_string()).unwrap();
        let (endpoint, server) = token_server(|| {});
        let result = refresh_claude(&path, &endpoint).unwrap();
        server.join().unwrap();
        assert_eq!(result["claudeAiOauth"]["refreshToken"], "rotated");
        assert_eq!(result["other"]["keep"], true);
        assert_eq!(object(&path).unwrap(), result);
        assert!(!dir.join(CLAUDE_PENDING).exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_file(dir.join(".miniutils-refresh.lock")).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    /// Answers one token request with a rotated login, after running `meanwhile`.
    fn token_server(meanwhile: impl FnOnce() + Send + 'static) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).unwrap();
            meanwhile();
            let body = r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        (endpoint, server)
    }

    #[test]
    fn rotated_claude_login_survives_a_failed_save() {
        let dir = std::env::temp_dir().join(format!("miniutils-refresh-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.json");
        let old = json!({"claudeAiOauth":{"accessToken":"old","refreshToken":"old-refresh","expiresAt":1},"other":{"keep":true}});
        std::fs::write(&path, old.to_string()).unwrap();
        // another program rewrites an unrelated field while the request is in flight: the
        // save is refused, but the rotated login is used and kept
        let changed = json!({"claudeAiOauth":{"accessToken":"old","refreshToken":"old-refresh","expiresAt":1},"other":{"keep":false}});
        let (endpoint, server) = token_server({ let path = path.clone(); let changed = changed.to_string(); move || std::fs::write(&path, changed).unwrap() });
        let result = refresh_claude(&path, &endpoint).unwrap();
        server.join().unwrap();
        assert_eq!(result["claudeAiOauth"]["refreshToken"], "rotated");
        assert_eq!(object(&path).unwrap(), changed);
        assert!(dir.join(CLAUDE_PENDING).exists());
        // the next call saves the kept login (merged into the current file) without a new
        // request: this endpoint doesn't answer
        let result = refresh_claude(&path, "http://127.0.0.1:9/token").unwrap();
        assert_eq!(result["claudeAiOauth"]["refreshToken"], "rotated");
        assert_eq!(result["other"]["keep"], false);
        assert_eq!(object(&path).unwrap(), result);
        assert!(!dir.join(CLAUDE_PENDING).exists());
        // a kept login for a refresh token that is no longer on disk is discarded
        std::fs::write(dir.join(CLAUDE_PENDING), json!({"replaces":"gone","login":{"claudeAiOauth":{"refreshToken":"stale"}}}).to_string()).unwrap();
        assert_eq!(pending_claude(&dir.join(CLAUDE_PENDING), &result), None);
        assert!(!dir.join(CLAUDE_PENDING).exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failure_kinds() {
        assert_eq!(Failure::from("x").kind, FailureKind::Other);
        assert_eq!(auth("x").kind, FailureKind::Auth);
        assert_eq!(auth("x").to_string(), "x");
    }

    #[test]
    #[ignore = "queries the services using the local logins"]
    fn local_direct_integrations() {
        for (name, result) in [("Claude", claude()), ("Codex", codex()), ("Kiro", kiro()), ("OpenCode Go", opencode())] {
            let card = result.unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!card.windows.is_empty());
            eprintln!("{name}: {} window(s)", card.windows.len());
        }
    }
}
