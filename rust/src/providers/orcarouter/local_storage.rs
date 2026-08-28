use std::collections::HashMap;

use chromium_storage_localstorage_core::{LocalStorageRecord, read_dir};

use crate::browser::detection::BrowserDetector;

const ORCA_ORIGINS: &[&str] = &["https://www.orcarouter.ai", "https://orcarouter.ai"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserIdentity {
    pub user_id: String,
    pub workspace_id: Option<String>,
}

fn is_orca_origin(origin: &str) -> bool {
    ORCA_ORIGINS
        .iter()
        .any(|candidate| origin.eq_ignore_ascii_case(candidate))
}

fn parse_user_id(value: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(value).ok()?;
    let id = json.get("id")?;
    match id {
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        _ => None,
    }
}

fn parse_workspace_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Number(number)) => Some(number.to_string()),
        Ok(serde_json::Value::String(text)) if !text.trim().is_empty() => {
            Some(text.trim().to_string())
        }
        _ if trimmed.chars().all(|ch| ch.is_ascii_digit()) => Some(trimmed.to_string()),
        _ => None,
    }
}

fn identities_from_records(records: &[LocalStorageRecord]) -> Vec<BrowserIdentity> {
    // The forensic reader intentionally surfaces superseded values and tombstones.
    // Resolve the latest `user` and `active_workspace_id` records per origin before
    // parsing them so stale login/workspace state cannot win over a later update.
    let mut latest_user: HashMap<String, (u64, bool, Option<String>)> = HashMap::new();
    let mut latest_workspace: HashMap<String, (u64, bool, Option<String>)> = HashMap::new();

    for record in records {
        let LocalStorageRecord::Data {
            origin,
            script_key,
            value,
            seq,
            deleted,
        } = record
        else {
            continue;
        };
        if !is_orca_origin(origin) || script_key.lossy {
            continue;
        }

        let (target, parsed) = match script_key.text.as_str() {
            "user" => (&mut latest_user, parse_user_id(&value.text)),
            "active_workspace_id" => (&mut latest_workspace, parse_workspace_id(&value.text)),
            _ => continue,
        };
        let replace = target
            .get(origin)
            .is_none_or(|(latest_seq, _, _)| seq > latest_seq);
        if replace {
            target.insert(
                origin.clone(),
                (
                    *seq,
                    *deleted,
                    if *deleted || value.lossy {
                        None
                    } else {
                        parsed
                    },
                ),
            );
        }
    }

    let mut identities = Vec::new();
    for (origin, (_, deleted, id)) in latest_user {
        if !deleted && let Some(id) = id {
            let workspace_id =
                latest_workspace
                    .get(&origin)
                    .and_then(|(_, workspace_deleted, workspace_id)| {
                        (!*workspace_deleted)
                            .then(|| workspace_id.clone())
                            .flatten()
                    });
            let identity = BrowserIdentity {
                user_id: id,
                workspace_id,
            };
            if !identities.contains(&identity) {
                identities.push(identity);
            }
        }
    }
    identities
}

pub(crate) fn browser_identities() -> Vec<BrowserIdentity> {
    let mut identities = Vec::new();

    for browser in BrowserDetector::detect_all() {
        if !browser.browser_type.is_chromium_based() {
            continue;
        }
        for profile in browser.profiles {
            let storage_dir = profile.path.join("Local Storage").join("leveldb");
            if !storage_dir.is_dir() {
                continue;
            }
            match read_dir(&storage_dir) {
                Ok(records) => {
                    let profile_identities = identities_from_records(&records);
                    tracing::debug!(
                        browser = %browser.browser_type.display_name(),
                        profile = %profile.name,
                        record_count = records.len(),
                        candidate_count = profile_identities.len(),
                        "OrcaRouter LocalStorage session probe decoded profile"
                    );
                    for identity in profile_identities {
                        if !identities.contains(&identity) {
                            identities.push(identity);
                        }
                    }
                }
                Err(error) => tracing::debug!(
                    browser = %browser.browser_type.display_name(),
                    profile = %profile.name,
                    %error,
                    "OrcaRouter LocalStorage session probe skipped unreadable profile"
                ),
            }
        }
    }

    identities
}

#[cfg(test)]
mod tests {
    use super::*;
    use chromium_storage_localstorage_core::{Encoding, StorageValue};

    fn value(text: &str) -> StorageValue {
        StorageValue {
            text: text.to_string(),
            raw: Vec::new(),
            encoding: Encoding::Latin1,
            lossy: false,
        }
    }

    fn record(seq: u64, deleted: bool, key: &str, text: &str) -> LocalStorageRecord {
        LocalStorageRecord::Data {
            origin: "https://www.orcarouter.ai".to_string(),
            script_key: value(key),
            value: value(text),
            seq,
            deleted,
        }
    }

    #[test]
    fn latest_user_record_wins_over_stale_values() {
        let records = vec![
            record(10, false, "user", r#"{"id":111}"#),
            record(12, false, "user", r#"{"id":222}"#),
            record(13, false, "active_workspace_id", "333"),
        ];
        assert_eq!(
            identities_from_records(&records),
            vec![BrowserIdentity {
                user_id: "222".to_string(),
                workspace_id: Some("333".to_string()),
            }]
        );
    }

    #[test]
    fn latest_tombstone_prevents_stale_login_reuse() {
        let records = vec![
            record(10, false, "user", r#"{"id":111}"#),
            record(12, true, "user", ""),
        ];
        assert!(identities_from_records(&records).is_empty());
    }

    #[test]
    fn workspace_tombstone_does_not_reuse_stale_workspace() {
        let records = vec![
            record(10, false, "user", r#"{"id":111}"#),
            record(11, false, "active_workspace_id", "222"),
            record(12, true, "active_workspace_id", ""),
        ];
        assert_eq!(
            identities_from_records(&records),
            vec![BrowserIdentity {
                user_id: "111".to_string(),
                workspace_id: None,
            }]
        );
    }

    #[test]
    fn parses_numeric_and_string_user_ids_only() {
        assert_eq!(parse_user_id(r#"{"id":123}"#).as_deref(), Some("123"));
        assert_eq!(parse_user_id(r#"{"id":"abc"}"#).as_deref(), Some("abc"));
        assert!(parse_user_id(r#"{"name":"missing"}"#).is_none());
        assert!(parse_user_id("not-json").is_none());
    }

    #[test]
    fn parses_numeric_and_string_workspace_ids() {
        assert_eq!(parse_workspace_id("123").as_deref(), Some("123"));
        assert_eq!(parse_workspace_id(r#""456""#).as_deref(), Some("456"));
        assert!(parse_workspace_id("").is_none());
        assert!(parse_workspace_id("not-a-number").is_none());
    }
}
