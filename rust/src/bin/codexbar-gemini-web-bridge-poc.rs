use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const HOST_NAME: &str = "com.codexbar.gemini_web_bridge_poc";
const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct GeminiAppsPush {
    version: u32,
    provider: String,
    observed_at: i64,
    payload: GeminiAppsPayload,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct GeminiAppsPayload {
    account_id: String,
    plan: Option<String>,
    source: String,
    current: QuotaWindow,
    weekly: QuotaWindow,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct QuotaWindow {
    label: String,
    used_percent: f64,
    resets_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct GeminiApiSpendPush {
    version: u32,
    provider: String,
    observed_at: i64,
    payload: GeminiApiSpendPayload,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct GeminiApiSpendPayload {
    used: f64,
    cap_used: Option<f64>,
    limit: Option<f64>,
    currency: String,
    period: String,
    resets_at: Option<String>,
    scope: Option<String>,
    source: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct AiHubMixRechargePush {
    version: u32,
    provider: String,
    observed_at: i64,
    payload: AiHubMixRechargePayload,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct AiHubMixRechargePayload {
    funded_balance_usd: f64,
    funding_created_at: Option<i64>,
    source: String,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(untagged)]
enum BrowserPush {
    GeminiApps(GeminiAppsPush),
    GeminiApi(GeminiApiSpendPush),
    AiHubMix(AiHubMixRechargePush),
}

impl BrowserPush {
    fn provider(&self) -> &str {
        match self {
            Self::GeminiApps(push) => &push.provider,
            Self::GeminiApi(push) => &push.provider,
            Self::AiHubMix(push) => &push.provider,
        }
    }

    fn observed_at(&self) -> i64 {
        match self {
            Self::GeminiApps(push) => push.observed_at,
            Self::GeminiApi(push) => push.observed_at,
            Self::AiHubMix(push) => push.observed_at,
        }
    }
}

fn state_dir() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .map(|path| path.join("CodexBar").join("gemini-web-bridge-poc"))
        .ok_or_else(|| "could not locate LOCALAPPDATA".to_string())
}

fn valid_extension_origin(origin: &str) -> bool {
    let Some(id) = origin
        .strip_prefix("chrome-extension://")
        .and_then(|value| value.strip_suffix('/'))
    else {
        return false;
    };
    id.len() == 32 && id.chars().all(|character| matches!(character, 'a'..='p'))
}

fn parse_allowed_origins(value: &str) -> Vec<String> {
    let mut origins = value
        .lines()
        .map(str::trim)
        .filter(|origin| valid_extension_origin(origin))
        .map(str::to_string)
        .collect::<Vec<_>>();
    origins.sort();
    origins.dedup();
    origins
}

fn expected_origins() -> Result<Vec<String>, String> {
    let state = state_dir()?;
    let multi_path = state.join("allowed-origins.txt");
    let legacy_path = state.join("allowed-origin.txt");
    let (path, value) = match fs::read_to_string(&multi_path) {
        Ok(value) => (multi_path, value),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let value = fs::read_to_string(&legacy_path)
                .map_err(|error| format!("cannot read {}: {error}", legacy_path.display()))?;
            (legacy_path, value)
        }
        Err(error) => return Err(format!("cannot read {}: {error}", multi_path.display())),
    };
    let origins = parse_allowed_origins(&value);
    if origins.is_empty() {
        return Err(format!(
            "{} contains no valid extension origins",
            path.display()
        ));
    }
    Ok(origins)
}

fn cache_path(provider: &str) -> Result<PathBuf, String> {
    let filename = match provider {
        "gemini-apps" => "gemini-apps-browser.json",
        "gemini-api" => "gemini-api-spend-browser.json",
        "aihubmix" => "aihubmix-recharge-browser.json",
        other => return Err(format!("unsupported provider: {other}")),
    };
    dirs::data_local_dir()
        .map(|path| path.join("CodexBar").join(filename))
        .ok_or_else(|| "could not locate LOCALAPPDATA".to_string())
}

fn validate_reset(value: &Option<String>) -> bool {
    value.as_ref().is_none_or(|timestamp| {
        timestamp.len() <= 64 && DateTime::parse_from_rfc3339(timestamp).is_ok()
    })
}

fn validate_window(window: &QuotaWindow, expected_label: &str) -> bool {
    window.label == expected_label
        && window.used_percent.is_finite()
        && (0.0..=100.0).contains(&window.used_percent)
        && validate_reset(&window.resets_at)
}

fn validate_gemini_apps_push(push: &GeminiAppsPush) -> Result<(), String> {
    if push.version != 1 {
        return Err(format!("unsupported message version: {}", push.version));
    }
    if push.provider != "gemini-apps" {
        return Err(format!("unsupported provider: {}", push.provider));
    }
    if push.observed_at <= 0 {
        return Err("observed_at must be a positive Unix timestamp".to_string());
    }
    if push.payload.account_id.is_empty()
        || push.payload.account_id.len() > 8
        || !push
            .payload
            .account_id
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return Err("account_id must be an ASCII numeric Gemini account slot".to_string());
    }
    if push
        .payload
        .plan
        .as_ref()
        .is_some_and(|plan| plan.is_empty() || plan.len() > 64)
    {
        return Err("plan must be absent or 1..=64 bytes".to_string());
    }
    if !matches!(push.payload.source.as_str(), "jSf9Qc" | "VxUbXb" | "dom") {
        return Err(format!(
            "unsupported parser source: {}",
            push.payload.source
        ));
    }
    if !validate_window(&push.payload.current, "Current usage") {
        return Err("invalid Current usage window".to_string());
    }
    if !validate_window(&push.payload.weekly, "Weekly limit") {
        return Err("invalid Weekly limit window".to_string());
    }
    Ok(())
}

fn parse_push(body: &[u8]) -> Result<BrowserPush, String> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|error| format!("invalid browser message JSON: {error}"))?;
    match value.get("provider").and_then(Value::as_str) {
        Some("gemini-apps") => {
            let push: GeminiAppsPush = serde_json::from_value(value)
                .map_err(|error| format!("invalid Gemini Apps browser message: {error}"))?;
            validate_gemini_apps_push(&push)?;
            Ok(BrowserPush::GeminiApps(push))
        }
        Some("gemini-api") => {
            let push: GeminiApiSpendPush = serde_json::from_value(value)
                .map_err(|error| format!("invalid Gemini API browser message: {error}"))?;
            validate_gemini_api_push(&push)?;
            Ok(BrowserPush::GeminiApi(push))
        }
        Some("aihubmix") => {
            let push: AiHubMixRechargePush = serde_json::from_value(value)
                .map_err(|error| format!("invalid AIHubMix browser message: {error}"))?;
            validate_aihubmix_push(&push)?;
            Ok(BrowserPush::AiHubMix(push))
        }
        Some(provider) => Err(format!("unsupported provider: {provider}")),
        None => Err("browser message is missing provider".to_string()),
    }
}

fn write_cache(push: &BrowserPush) -> Result<PathBuf, String> {
    let path = cache_path(push.provider())?;
    let parent = path
        .parent()
        .ok_or_else(|| "cache path has no parent".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(push)
        .map_err(|error| format!("cannot serialize cache: {error}"))?;
    codexbar::cli::dashboard::write_atomic(&path, &bytes)
        .map_err(|error| format!("cannot atomically write {}: {error}", path.display()))?;
    Ok(path)
}

fn validate_gemini_api_push(push: &GeminiApiSpendPush) -> Result<(), String> {
    if push.version != 1 || push.provider != "gemini-api" {
        return Err("unsupported Gemini API message version/provider".to_string());
    }
    if push.observed_at <= 0 {
        return Err("observed_at must be a positive Unix timestamp".to_string());
    }
    let payload = &push.payload;
    if !payload.used.is_finite() || payload.used < 0.0 {
        return Err("used must be a finite non-negative amount".to_string());
    }
    if payload
        .cap_used
        .is_some_and(|used| !used.is_finite() || used < 0.0)
    {
        return Err("cap_used must be absent or a finite non-negative amount".to_string());
    }
    if payload
        .limit
        .is_some_and(|limit| !limit.is_finite() || limit <= 0.0)
    {
        return Err("limit must be absent or a finite positive amount".to_string());
    }
    if payload.limit.is_some() && payload.cap_used.is_none() {
        return Err("a configured limit requires cap_used".to_string());
    }
    if !matches!(payload.currency.as_str(), "USD" | "EUR" | "GBP" | "JPY") {
        return Err("unsupported Gemini API currency".to_string());
    }
    if payload.period.is_empty()
        || payload.period.len() > 64
        || payload.period.contains('@')
        || payload
            .period
            .to_ascii_lowercase()
            .contains("billing account")
        || payload.period.to_ascii_lowercase().contains("account id")
    {
        return Err("period must be 1..=64 bytes".to_string());
    }
    if !validate_reset(&payload.resets_at) {
        return Err("invalid Gemini API reset timestamp".to_string());
    }
    if payload.scope.as_ref().is_some_and(|scope| {
        let lower = scope.to_ascii_lowercase();
        scope.is_empty()
            || scope.len() > 64
            || scope.contains('@')
            || lower.contains("billing")
            || lower.contains("account")
    }) {
        return Err("scope is not a safe display label".to_string());
    }
    if payload.source != "dom" {
        return Err("unsupported Gemini API parser source".to_string());
    }
    Ok(())
}

fn validate_aihubmix_push(push: &AiHubMixRechargePush) -> Result<(), String> {
    if push.version != 1 || push.provider != "aihubmix" {
        return Err("unsupported AIHubMix message version/provider".to_string());
    }
    if push.observed_at <= 0 {
        return Err("observed_at must be a positive Unix timestamp".to_string());
    }
    let payload = &push.payload;
    if !payload.funded_balance_usd.is_finite() || payload.funded_balance_usd <= 0.0 {
        return Err("funded_balance_usd must be a finite positive amount".to_string());
    }
    if payload
        .funding_created_at
        .is_some_and(|timestamp| timestamp <= 0 || timestamp > push.observed_at + 60)
    {
        return Err("funding_created_at must be a plausible Unix timestamp".to_string());
    }
    if !matches!(payload.source.as_str(), "network" | "dom") {
        return Err("unsupported AIHubMix parser source".to_string());
    }
    Ok(())
}

fn read_frame(reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut prefix = [0_u8; 4];
    let first = reader.read(&mut prefix[..1])?;
    if first == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut prefix[1..])?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("native message length {length} is outside 1..={MAX_FRAME_BYTES}"),
        ));
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

fn write_frame(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    let length = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "response is too large"))?;
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(&body)?;
    writer.flush()
}

fn run(origin: &str) -> Result<(), String> {
    let expected = expected_origins()?;
    if !expected.iter().any(|allowed| allowed == origin) {
        return Err(format!("extension origin is not allowed: {origin}"));
    }

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    loop {
        let Some(body) = read_frame(&mut reader).map_err(|error| error.to_string())? else {
            return Ok(());
        };
        let response = match parse_push(&body).and_then(|push| {
            let path = write_cache(&push)?;
            Ok((push, path))
        }) {
            Ok((push, path)) => serde_json::json!({
                "ok": true,
                "provider": push.provider(),
                "observed_at": push.observed_at(),
                "cache": path.file_name().and_then(|name| name.to_str())
            }),
            Err(error) => serde_json::json!({ "ok": false, "error": error }),
        };
        write_frame(&mut writer, &response).map_err(|error| error.to_string())?;
    }
}

fn main() {
    let Some(origin) = std::env::args().nth(1) else {
        eprintln!("{HOST_NAME}: expected Chromium extension origin argument");
        std::process::exit(2);
    };
    if !origin.starts_with("chrome-extension://") {
        eprintln!("{HOST_NAME}: invalid native host origin");
        std::process::exit(2);
    }
    if let Err(error) = run(&origin) {
        eprintln!("{HOST_NAME}: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn valid_json() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "provider": "gemini-apps",
            "observed_at": 1_787_990_400,
            "payload": {
                "account_id": "0",
                "plan": "Pro",
                "source": "jSf9Qc",
                "current": {
                    "label": "Current usage",
                    "used_percent": 25.0,
                    "resets_at": "2026-08-29T12:00:00Z"
                },
                "weekly": {
                    "label": "Weekly limit",
                    "used_percent": 10.0,
                    "resets_at": "2026-09-03T12:00:00Z"
                }
            }
        }))
        .unwrap()
    }

    fn valid_spend_json() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "provider": "gemini-api",
            "observed_at": 1_788_400_000,
            "payload": {
                "used": 5.0,
                "cap_used": 12.5,
                "limit": 50.0,
                "currency": "USD",
                "period": "Current month",
                "resets_at": null,
                "scope": "Project Demo",
                "source": "dom"
            }
        }))
        .unwrap()
    }

    fn valid_aihubmix_json() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "provider": "aihubmix",
            "observed_at": 1_788_500_000,
            "payload": {
                "funded_balance_usd": 10.0,
                "funding_created_at": 1_788_400_000,
                "source": "network"
            }
        }))
        .unwrap()
    }

    #[test]
    fn accepts_only_the_secret_free_wire_contract() {
        let push = parse_push(&valid_json()).unwrap();
        let BrowserPush::GeminiApps(push) = push else {
            panic!("wrong provider")
        };
        assert_eq!(push.provider, "gemini-apps");
        assert_eq!(push.payload.current.used_percent, 25.0);
        assert_eq!(push.payload.weekly.used_percent, 10.0);
    }

    #[test]
    fn accepts_sanitized_gemini_api_spend_contract() {
        let push = parse_push(&valid_spend_json()).unwrap();
        let BrowserPush::GeminiApi(push) = push else {
            panic!("wrong provider")
        };
        assert_eq!(push.payload.used, 5.0);
        assert_eq!(push.payload.cap_used, Some(12.5));
        assert_eq!(push.payload.limit, Some(50.0));
        assert_eq!(push.payload.currency, "USD");
    }

    #[test]
    fn accepts_sanitized_aihubmix_recharge_contract() {
        let push = parse_push(&valid_aihubmix_json()).unwrap();
        let BrowserPush::AiHubMix(push) = push else {
            panic!("wrong provider")
        };
        assert_eq!(push.payload.funded_balance_usd, 10.0);
        assert_eq!(push.payload.funding_created_at, Some(1_788_400_000));
        assert_eq!(push.payload.source, "network");
    }

    #[test]
    fn unknown_token_or_cookie_fields_are_rejected() {
        let mut value: Value = serde_json::from_slice(&valid_json()).unwrap();
        value["payload"]["token"] = Value::String("must-not-cross-boundary".to_string());
        assert!(parse_push(&serde_json::to_vec(&value).unwrap()).is_err());

        let mut value: Value = serde_json::from_slice(&valid_spend_json()).unwrap();
        value["payload"]["billing_account_id"] =
            Value::String("must-not-cross-boundary".to_string());
        assert!(parse_push(&serde_json::to_vec(&value).unwrap()).is_err());

        let mut value: Value = serde_json::from_slice(&valid_json()).unwrap();
        value["payload"]["cookie"] = Value::String("must-not-cross-boundary".to_string());
        assert!(parse_push(&serde_json::to_vec(&value).unwrap()).is_err());

        for field in ["access_token", "authorization", "cookie", "records"] {
            let mut value: Value = serde_json::from_slice(&valid_aihubmix_json()).unwrap();
            value["payload"][field] = Value::String("must-not-cross-boundary".to_string());
            assert!(parse_push(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }

    #[test]
    fn invalid_percentages_and_partial_windows_fail_closed() {
        let mut value: Value = serde_json::from_slice(&valid_json()).unwrap();
        value["payload"]["current"]["used_percent"] = serde_json::json!(101.0);
        assert!(parse_push(&serde_json::to_vec(&value).unwrap()).is_err());

        let mut value: Value = serde_json::from_slice(&valid_json()).unwrap();
        value["payload"].as_object_mut().unwrap().remove("weekly");
        assert!(parse_push(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn native_frames_are_length_bounded_and_round_trip() {
        let body = valid_json();
        let mut framed = Vec::new();
        framed.extend_from_slice(&(body.len() as u32).to_le_bytes());
        framed.extend_from_slice(&body);
        assert_eq!(read_frame(&mut Cursor::new(framed)).unwrap().unwrap(), body);

        let prefix = ((MAX_FRAME_BYTES + 1) as u32).to_le_bytes();
        assert_eq!(
            read_frame(&mut Cursor::new(prefix)).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn multiple_extension_origins_are_strictly_validated_and_deduplicated() {
        let first = "chrome-extension://adakkbinjhpjcpiagphgeldfmcmmpdcc/";
        let second = "chrome-extension://bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb/";
        let origins = parse_allowed_origins(&format!(
            "{first}\ninvalid\n{second}\n{first}\nchrome-extension://ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ/\n"
        ));
        assert_eq!(origins, vec![first.to_string(), second.to_string()]);
    }
}
