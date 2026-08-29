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
struct BrowserPush {
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

fn state_dir() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .map(|path| path.join("CodexBar").join("gemini-web-bridge-poc"))
        .ok_or_else(|| "could not locate LOCALAPPDATA".to_string())
}

fn expected_origin() -> Result<String, String> {
    let path = state_dir()?.join("allowed-origin.txt");
    fs::read_to_string(&path)
        .map(|value| value.trim().to_string())
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}

fn cache_path() -> Result<PathBuf, String> {
    dirs::data_local_dir()
        .map(|path| path.join("CodexBar").join("gemini-web-bridge-poc.json"))
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

fn validate_push(push: &BrowserPush) -> Result<(), String> {
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
    let push: BrowserPush = serde_json::from_slice(body)
        .map_err(|error| format!("invalid browser message JSON: {error}"))?;
    validate_push(&push)?;
    Ok(push)
}

fn write_cache(push: &BrowserPush) -> Result<PathBuf, String> {
    let path = cache_path()?;
    let parent = path
        .parent()
        .ok_or_else(|| "cache path has no parent".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(push)
        .map_err(|error| format!("cannot serialize cache: {error}"))?;
    fs::write(&path, bytes).map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    Ok(path)
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
    let expected = expected_origin()?;
    if origin != expected {
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
                "provider": push.provider,
                "observed_at": push.observed_at,
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

    #[test]
    fn accepts_only_the_secret_free_wire_contract() {
        let push = parse_push(&valid_json()).unwrap();
        assert_eq!(push.provider, "gemini-apps");
        assert_eq!(push.payload.current.used_percent, 25.0);
        assert_eq!(push.payload.weekly.used_percent, 10.0);
    }

    #[test]
    fn unknown_token_or_cookie_fields_are_rejected() {
        let mut value: Value = serde_json::from_slice(&valid_json()).unwrap();
        value["payload"]["token"] = Value::String("must-not-cross-boundary".to_string());
        assert!(parse_push(&serde_json::to_vec(&value).unwrap()).is_err());

        let mut value: Value = serde_json::from_slice(&valid_json()).unwrap();
        value["payload"]["cookie"] = Value::String("must-not-cross-boundary".to_string());
        assert!(parse_push(&serde_json::to_vec(&value).unwrap()).is_err());
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
}
