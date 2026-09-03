//! Gemini API spend provider backed by the local AI Studio Browser Bridge cache.
//!
//! The browser side sends only current-period spend metadata. Cookies, WIZ
//! state, billing identifiers, emails, raw HTML and raw network payloads never
//! cross the Native Messaging boundary.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::core::{
    CostSnapshot, FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId,
    ProviderMetadata, RateWindow, SourceMode, UsageSnapshot,
};

const CACHE_FILENAME: &str = "gemini-api-spend-browser.json";
const MAX_CACHE_AGE_SECONDS: i64 = 24 * 60 * 60;
const CLOCK_SKEW_SECONDS: i64 = 60;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BrowserCache {
    version: u32,
    provider: String,
    observed_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport: Option<String>,
    payload: SpendPayload,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpendPayload {
    used: f64,
    #[serde(default)]
    cap_used: Option<f64>,
    limit: Option<f64>,
    currency: String,
    period: String,
    resets_at: Option<String>,
    scope: Option<String>,
    source: String,
}

pub struct GeminiApiProvider {
    metadata: ProviderMetadata,
}

impl GeminiApiProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::GeminiApi,
                display_name: "Gemini API",
                session_label: "Monthly spend cap",
                weekly_label: "Spend",
                supports_opus: false,
                supports_credits: true,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some("https://aistudio.google.com/spend"),
                status_page_url: Some("https://status.cloud.google.com"),
            },
        }
    }

    fn cache_path() -> Result<PathBuf, ProviderError> {
        dirs::data_local_dir()
            .map(|root| root.join("CodexBar").join(CACHE_FILENAME))
            .ok_or_else(|| {
                ProviderError::NotInstalled(
                    "Could not locate LOCALAPPDATA for Gemini API spend cache.".into(),
                )
            })
    }

    fn read_cache() -> Result<String, ProviderError> {
        let path = Self::cache_path()?;
        fs::read_to_string(&path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ProviderError::NotInstalled(
                    "Gemini API Browser Bridge has not produced a spend snapshot yet. Open a signed-in AI Studio Spend tab with the bridge enabled."
                        .into(),
                )
            } else {
                ProviderError::Other(format!(
                    "Failed to read Gemini API Browser Bridge cache {}: {error}",
                    path.display()
                ))
            }
        })
    }

    fn parse_reset(value: Option<&str>) -> Result<Option<DateTime<Utc>>, ProviderError> {
        value
            .map(|raw| {
                DateTime::parse_from_rfc3339(raw)
                    .map(|date| date.with_timezone(&Utc))
                    .map_err(|error| {
                        ProviderError::Parse(format!(
                            "Invalid Gemini API spend reset timestamp: {error}"
                        ))
                    })
            })
            .transpose()
    }

    fn validate_payload(payload: &SpendPayload) -> Result<(), ProviderError> {
        if !payload.used.is_finite() || payload.used < 0.0 {
            return Err(ProviderError::Parse(
                "Invalid Gemini API spend amount".into(),
            ));
        }
        if payload
            .cap_used
            .is_some_and(|used| !used.is_finite() || used < 0.0)
        {
            return Err(ProviderError::Parse(
                "Invalid Gemini API spend-cap current amount".into(),
            ));
        }
        if payload
            .limit
            .is_some_and(|limit| !limit.is_finite() || limit <= 0.0)
        {
            return Err(ProviderError::Parse(
                "Invalid Gemini API spend limit".into(),
            ));
        }
        if payload.limit.is_some() && payload.cap_used.is_none() {
            return Err(ProviderError::Parse(
                "Gemini API spend cap is missing its current amount".into(),
            ));
        }
        if !matches!(payload.currency.as_str(), "USD" | "EUR" | "GBP" | "JPY") {
            return Err(ProviderError::Parse(
                "Invalid Gemini API currency code".into(),
            ));
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
            return Err(ProviderError::Parse(
                "Invalid Gemini API spend period".into(),
            ));
        }
        if payload.scope.as_ref().is_some_and(|scope| {
            let lower = scope.to_ascii_lowercase();
            scope.is_empty()
                || scope.len() > 64
                || scope.contains('@')
                || lower.contains("billing")
                || lower.contains("account")
        }) {
            return Err(ProviderError::Parse(
                "Invalid Gemini API scope label".into(),
            ));
        }
        if payload.source != "dom" {
            return Err(ProviderError::Parse(
                "Unknown Gemini API Browser Bridge parser source".into(),
            ));
        }
        Ok(())
    }

    fn result_from_payload(
        payload: SpendPayload,
        observed_at: DateTime<Utc>,
        source_label: &'static str,
    ) -> Result<ProviderFetchResult, ProviderError> {
        Self::validate_payload(&payload)?;
        let resets_at = Self::parse_reset(payload.resets_at.as_deref())?;
        let mut cost = CostSnapshot::new(payload.used, payload.currency, payload.period);
        cost.resets_at = resets_at;
        cost.updated_at = observed_at;

        let primary = match (payload.cap_used, payload.limit) {
            (Some(cap_used), Some(limit)) => RateWindow::new((cap_used / limit) * 100.0),
            _ => RateWindow::informational("Spend only"),
        };
        let mut usage = UsageSnapshot::new(primary);
        usage.updated_at = observed_at;
        if let Some(scope) = payload.scope {
            usage = usage.with_login_method(scope);
        }
        Ok(ProviderFetchResult::new(usage, source_label).with_cost(cost))
    }

    fn result_from_cache(
        raw: &str,
        now: DateTime<Utc>,
    ) -> Result<ProviderFetchResult, ProviderError> {
        let cache: BrowserCache = serde_json::from_str(raw)
            .map_err(|error| ProviderError::Parse(format!("Invalid Gemini API cache: {error}")))?;
        if cache.version != 1 || cache.provider != "gemini-api" {
            return Err(ProviderError::Parse(
                "Unsupported Gemini API Browser Bridge cache version/provider".into(),
            ));
        }
        let observed_at =
            DateTime::<Utc>::from_timestamp(cache.observed_at, 0).ok_or_else(|| {
                ProviderError::Parse("Invalid Gemini API observation timestamp".into())
            })?;
        let age = (now - observed_at).num_seconds();
        if age < -CLOCK_SKEW_SECONDS {
            return Err(ProviderError::Other(
                "Gemini API Browser Bridge snapshot is from the future; check the system clock."
                    .into(),
            ));
        }
        if age > MAX_CACHE_AGE_SECONDS {
            return Err(ProviderError::Other(format!(
                "Gemini API Browser Bridge snapshot is stale ({age}s old). Keep a signed-in AI Studio Spend tab open so the bridge can refresh it."
            )));
        }
        let source_label = match cache.transport.as_deref() {
            None | Some("browser-bridge") => "browser-bridge",
            // Older builds could persist a sanitized snapshot after a CDP read.
            // Keep that cache readable during migration, but label it as cache
            // so it cannot be mistaken for a live DevTools connection.
            Some("browser-cdp") | Some("browser-cache") => "browser-cache",
            Some(other) => {
                return Err(ProviderError::Parse(format!(
                    "Unknown Gemini API cache transport: {other}"
                )));
            }
        };
        Self::result_from_payload(cache.payload, observed_at, source_label)
    }
}

impl Default for GeminiApiProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for GeminiApiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::GeminiApi
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        if !matches!(ctx.source_mode, SourceMode::Auto | SourceMode::Web) {
            return Err(ProviderError::UnsupportedSource(ctx.source_mode));
        }
        let now = Utc::now();
        Self::read_cache().and_then(|raw| Self::result_from_cache(&raw, now))
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto]
    }

    fn supports_web(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cache(observed_at: i64) -> String {
        json!({
            "version": 1,
            "provider": "gemini-api",
            "observed_at": observed_at,
            "payload": {
                "used": 5.0,
                "cap_used": 12.5,
                "limit": 50.0,
                "currency": "USD",
                "period": "Current month",
                "resets_at": "2026-10-01T00:00:00Z",
                "scope": "Project ab12cd34",
                "source": "dom"
            }
        })
        .to_string()
    }

    #[test]
    fn separates_net_cost_from_spend_cap_utilization() {
        let now = DateTime::parse_from_rfc3339("2026-09-03T06:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let result =
            GeminiApiProvider::result_from_cache(&cache(now.timestamp() - 30), now).unwrap();
        assert!(!result.usage.primary.is_informational);
        assert_eq!(result.usage.primary.used_percent, 25.0);
        let cost = result.cost.unwrap();
        assert_eq!(cost.used, 5.0);
        assert_eq!(cost.limit, None);
        assert_eq!(cost.currency_code, "USD");
        assert_eq!(cost.period, "Current month");
    }

    #[test]
    fn rejects_partial_unknown_or_secret_bearing_cache() {
        let now = DateTime::parse_from_rfc3339("2026-09-03T06:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut value: serde_json::Value = serde_json::from_str(&cache(now.timestamp())).unwrap();
        value["payload"].as_object_mut().unwrap().remove("used");
        assert!(GeminiApiProvider::result_from_cache(&value.to_string(), now).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&cache(now.timestamp())).unwrap();
        value["payload"]["billing_account_id"] = json!("secret-id");
        assert!(GeminiApiProvider::result_from_cache(&value.to_string(), now).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&cache(now.timestamp())).unwrap();
        value["payload"]["source"] = json!("network");
        assert!(GeminiApiProvider::result_from_cache(&value.to_string(), now).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&cache(now.timestamp())).unwrap();
        value["payload"]["currency"] = json!("ABC");
        assert!(GeminiApiProvider::result_from_cache(&value.to_string(), now).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&cache(now.timestamp())).unwrap();
        value["payload"]["scope"] = json!("person@example.com");
        assert!(GeminiApiProvider::result_from_cache(&value.to_string(), now).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&cache(now.timestamp())).unwrap();
        value["payload"]["period"] = json!("person@example.com");
        assert!(GeminiApiProvider::result_from_cache(&value.to_string(), now).is_err());

        let mut value: serde_json::Value = serde_json::from_str(&cache(now.timestamp())).unwrap();
        value["payload"].as_object_mut().unwrap().remove("cap_used");
        assert!(GeminiApiProvider::result_from_cache(&value.to_string(), now).is_err());
    }

    #[test]
    fn no_configured_cap_remains_cost_only() {
        let now = DateTime::parse_from_rfc3339("2026-09-03T06:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut value: serde_json::Value = serde_json::from_str(&cache(now.timestamp())).unwrap();
        value["payload"]["cap_used"] = serde_json::Value::Null;
        value["payload"]["limit"] = serde_json::Value::Null;
        let result = GeminiApiProvider::result_from_cache(&value.to_string(), now).unwrap();
        assert!(result.usage.primary.is_informational);
        assert_eq!(result.cost.unwrap().used, 5.0);
    }

    #[test]
    fn periodic_provider_refresh_exposes_cache_only_auto_source() {
        let provider = GeminiApiProvider::new();
        assert_eq!(provider.available_sources(), vec![SourceMode::Auto]);
        assert!(!provider.supports_web());
    }

    #[test]
    fn legacy_cdp_snapshot_is_read_only_cache_not_a_live_cdp_source() {
        let now = DateTime::parse_from_rfc3339("2026-09-03T06:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut value: serde_json::Value = serde_json::from_str(&cache(now.timestamp())).unwrap();
        value["transport"] = json!("browser-cdp");
        let result = GeminiApiProvider::result_from_cache(&value.to_string(), now).unwrap();
        assert_eq!(result.source_label, "browser-cache");
    }
}
