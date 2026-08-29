//! Gemini Apps provider backed by the local Browser Bridge cache.
//!
//! Authentication stays inside the signed-in `gemini.google.com` browser tab.
//! CodexBar reads only a sanitized cache containing percentages, reset times,
//! plan label, account slot, parser source, and observation time.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

use crate::core::{
    FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId, ProviderMetadata,
    RateWindow, SourceMode, UsageSnapshot,
};

const STABLE_CACHE_FILENAME: &str = "gemini-apps-browser.json";
const LEGACY_POC_CACHE_FILENAME: &str = "gemini-web-bridge-poc.json";
const MAX_CACHE_AGE_SECONDS: i64 = 24 * 60 * 60;
const CLOCK_SKEW_SECONDS: i64 = 60;
const CURRENT_WINDOW_MINUTES: u32 = 5 * 60;
const WEEKLY_WINDOW_MINUTES: u32 = 7 * 24 * 60;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserCache {
    version: u32,
    provider: String,
    observed_at: i64,
    payload: GeminiAppsPayload,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeminiAppsPayload {
    account_id: String,
    plan: Option<String>,
    source: String,
    current: BrowserWindow,
    weekly: BrowserWindow,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserWindow {
    label: String,
    used_percent: f64,
    resets_at: Option<String>,
}

pub struct GeminiAppsProvider {
    metadata: ProviderMetadata,
}

impl GeminiAppsProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::GeminiApps,
                display_name: "Gemini Apps",
                session_label: "Current usage",
                weekly_label: "Weekly limit",
                supports_opus: false,
                supports_credits: false,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some("https://gemini.google.com/usage"),
                status_page_url: None,
            },
        }
    }

    fn cache_candidates() -> Result<[PathBuf; 2], ProviderError> {
        let root = dirs::data_local_dir().ok_or_else(|| {
            ProviderError::NotInstalled("Could not locate LOCALAPPDATA for Gemini Apps.".into())
        })?;
        let codexbar = root.join("CodexBar");
        Ok([
            codexbar.join(STABLE_CACHE_FILENAME),
            codexbar.join(LEGACY_POC_CACHE_FILENAME),
        ])
    }

    fn read_cache() -> Result<String, ProviderError> {
        for path in Self::cache_candidates()? {
            match fs::read_to_string(&path) {
                Ok(raw) => return Ok(raw),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(ProviderError::Other(format!(
                        "Failed to read Gemini Apps Browser Bridge cache {}: {error}",
                        path.display()
                    )));
                }
            }
        }

        Err(ProviderError::NotInstalled(
            "Gemini Apps Browser Bridge has not produced a usage snapshot yet. Open a signed-in Gemini Usage tab with the bridge enabled."
                .into(),
        ))
    }

    fn parse_reset(value: Option<&str>) -> Result<Option<DateTime<Utc>>, ProviderError> {
        value
            .map(|raw| {
                DateTime::parse_from_rfc3339(raw)
                    .map(|date| date.with_timezone(&Utc))
                    .map_err(|error| {
                        ProviderError::Parse(format!(
                            "Invalid Gemini Apps reset timestamp: {error}"
                        ))
                    })
            })
            .transpose()
    }

    fn validate_window(window: &BrowserWindow, expected_label: &str) -> Result<(), ProviderError> {
        if window.label != expected_label {
            return Err(ProviderError::Parse(format!(
                "Unexpected Gemini Apps meter label: {}",
                window.label
            )));
        }
        if !window.used_percent.is_finite() || !(0.0..=100.0).contains(&window.used_percent) {
            return Err(ProviderError::Parse(format!(
                "Invalid Gemini Apps percentage for {expected_label}"
            )));
        }
        Ok(())
    }

    fn usage_from_cache(raw: &str, now: DateTime<Utc>) -> Result<UsageSnapshot, ProviderError> {
        let cache: BrowserCache = serde_json::from_str(raw)
            .map_err(|error| ProviderError::Parse(format!("Invalid Gemini Apps cache: {error}")))?;

        if cache.version != 1 || cache.provider != "gemini-apps" {
            return Err(ProviderError::Parse(
                "Unsupported Gemini Apps Browser Bridge cache version/provider".into(),
            ));
        }
        if cache.payload.account_id.is_empty()
            || cache.payload.account_id.len() > 8
            || !cache
                .payload
                .account_id
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            return Err(ProviderError::Parse(
                "Invalid Gemini Apps account slot in Browser Bridge cache".into(),
            ));
        }
        if !matches!(cache.payload.source.as_str(), "jSf9Qc" | "VxUbXb" | "dom") {
            return Err(ProviderError::Parse(
                "Unknown Gemini Apps Browser Bridge parser source".into(),
            ));
        }
        if cache
            .payload
            .plan
            .as_ref()
            .is_some_and(|plan| plan.is_empty() || plan.len() > 64)
        {
            return Err(ProviderError::Parse(
                "Invalid Gemini Apps plan label in Browser Bridge cache".into(),
            ));
        }

        let observed_at =
            DateTime::<Utc>::from_timestamp(cache.observed_at, 0).ok_or_else(|| {
                ProviderError::Parse("Invalid Gemini Apps observation timestamp".into())
            })?;
        let age = (now - observed_at).num_seconds();
        if age < -CLOCK_SKEW_SECONDS {
            return Err(ProviderError::Other(
                "Gemini Apps Browser Bridge snapshot is from the future; check the system clock."
                    .into(),
            ));
        }
        if age > MAX_CACHE_AGE_SECONDS {
            return Err(ProviderError::Other(format!(
                "Gemini Apps Browser Bridge snapshot is stale ({age}s old). Keep a signed-in Gemini tab open so the background bridge can refresh it."
            )));
        }

        Self::validate_window(&cache.payload.current, "Current usage")?;
        Self::validate_window(&cache.payload.weekly, "Weekly limit")?;

        let current = RateWindow::with_details(
            cache.payload.current.used_percent,
            Some(CURRENT_WINDOW_MINUTES),
            Self::parse_reset(cache.payload.current.resets_at.as_deref())?,
            None,
        );
        let weekly = RateWindow::with_details(
            cache.payload.weekly.used_percent,
            Some(WEEKLY_WINDOW_MINUTES),
            Self::parse_reset(cache.payload.weekly.resets_at.as_deref())?,
            None,
        );

        let mut usage = UsageSnapshot::new(current).with_secondary(weekly);
        usage.updated_at = observed_at;
        if let Some(plan) = cache.payload.plan {
            usage = usage.with_login_method(plan);
        }
        Ok(usage)
    }

    #[cfg(test)]
    fn cache_path_for_test(root: &std::path::Path, stable: bool) -> PathBuf {
        root.join(if stable {
            STABLE_CACHE_FILENAME
        } else {
            LEGACY_POC_CACHE_FILENAME
        })
    }
}

impl Default for GeminiAppsProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for GeminiAppsProvider {
    fn id(&self) -> ProviderId {
        ProviderId::GeminiApps
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        if !matches!(ctx.source_mode, SourceMode::Auto | SourceMode::Web) {
            return Err(ProviderError::UnsupportedSource(ctx.source_mode));
        }
        let raw = Self::read_cache()?;
        let usage = Self::usage_from_cache(&raw, Utc::now())?;
        Ok(ProviderFetchResult::new(usage, "browser-bridge"))
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::Web]
    }

    fn supports_web(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cache(observed_at: i64) -> String {
        json!({
            "version": 1,
            "provider": "gemini-apps",
            "observed_at": observed_at,
            "payload": {
                "account_id": "0",
                "plan": "Pro",
                "source": "jSf9Qc",
                "current": {
                    "label": "Current usage",
                    "used_percent": 12.5,
                    "resets_at": "2026-08-29T12:00:00Z"
                },
                "weekly": {
                    "label": "Weekly limit",
                    "used_percent": 37.25,
                    "resets_at": "2026-09-03T12:00:00Z"
                }
            }
        })
        .to_string()
    }

    #[test]
    fn converts_bridge_cache_to_current_and_weekly_rate_windows() {
        let now = DateTime::parse_from_rfc3339("2026-08-29T11:55:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let usage =
            GeminiAppsProvider::usage_from_cache(&cache(now.timestamp() - 60), now).unwrap();

        assert_eq!(usage.primary.used_percent, 12.5);
        assert_eq!(usage.primary.window_minutes, Some(300));
        assert_eq!(usage.secondary.as_ref().unwrap().used_percent, 37.25);
        assert_eq!(
            usage.secondary.as_ref().unwrap().window_minutes,
            Some(10_080)
        );
        assert_eq!(usage.login_method.as_deref(), Some("Pro"));
        assert_eq!(usage.updated_at.timestamp(), now.timestamp() - 60);
    }

    #[test]
    fn rejects_stale_future_partial_and_secret_bearing_cache() {
        let now = DateTime::parse_from_rfc3339("2026-08-29T11:55:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(
            GeminiAppsProvider::usage_from_cache(
                &cache(now.timestamp() - MAX_CACHE_AGE_SECONDS - 1),
                now
            )
            .is_err()
        );
        assert!(
            GeminiAppsProvider::usage_from_cache(
                &cache(now.timestamp() + CLOCK_SKEW_SECONDS + 1),
                now
            )
            .is_err()
        );

        let mut value: serde_json::Value = serde_json::from_str(&cache(now.timestamp())).unwrap();
        value["payload"].as_object_mut().unwrap().remove("weekly");
        assert!(GeminiAppsProvider::usage_from_cache(&value.to_string(), now).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&cache(now.timestamp())).unwrap();
        value["payload"]["token"] = json!("must-not-cross-boundary");
        assert!(GeminiAppsProvider::usage_from_cache(&value.to_string(), now).is_err());
    }

    #[test]
    fn metadata_and_sources_are_browser_bridge_specific() {
        let provider = GeminiAppsProvider::new();
        assert_eq!(provider.id(), ProviderId::GeminiApps);
        assert_eq!(provider.metadata().display_name, "Gemini Apps");
        assert_eq!(provider.metadata().session_label, "Current usage");
        assert_eq!(provider.metadata().weekly_label, "Weekly limit");
        assert!(!provider.metadata().default_enabled);
        assert_eq!(
            provider.metadata().dashboard_url,
            Some("https://gemini.google.com/usage")
        );
        assert_eq!(
            provider.available_sources(),
            vec![SourceMode::Auto, SourceMode::Web]
        );
        assert!(provider.supports_web());
    }

    #[test]
    fn stable_cache_name_precedes_legacy_poc_name() {
        let root = std::path::Path::new("C:/cache");
        assert_eq!(
            GeminiAppsProvider::cache_path_for_test(root, true),
            root.join(STABLE_CACHE_FILENAME)
        );
        assert_eq!(
            GeminiAppsProvider::cache_path_for_test(root, false),
            root.join(LEGACY_POC_CACHE_FILENAME)
        );
    }
}
