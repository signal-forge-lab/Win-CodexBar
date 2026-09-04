//! AIHubMix account credit provider.
//!
//! AIHubMix exposes the current account balance through the documented
//! `GET /api/user/self` platform endpoint. The wire quota unit is 1 / 500000
//! USD. The signed-in console's recharge page is Clerk-session protected, so
//! the Browser Bridge captures only the newest funded `balance_after` value
//! and persists that secret-free scalar locally. Auto mode combines the live
//! Manage-Key balance with that cache to drive FloatBar depletion.
//!
//! Auto mode uses an explicitly configured AIHubMix Manage Key and never
//! initiates CDP. Explicit Web mode can use a signed-in local Chromium CDP
//! session; that path never extracts browser credentials and emits only
//! sanitized numeric quota fields to CodexBar.

mod cdp;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::{fs, path::PathBuf};

use crate::core::{
    CostSnapshot, FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId,
    ProviderMetadata, RateWindow, SourceMode, UsageSnapshot,
};

const SELF_URL: &str = "https://aihubmix.com/api/user/self";
const SELF_URL_FALLBACK: &str = "https://api.aihubmix.com/api/user/self";
const DASHBOARD_URL: &str = "https://console.aihubmix.com/topup";
const RECHARGE_CACHE_FILENAME: &str = "aihubmix-recharge-browser.json";
const RECHARGE_CACHE_MAX_AGE_SECONDS: i64 = 30 * 24 * 60 * 60;
const CLOCK_SKEW_SECONDS: i64 = 60;
const CREDENTIAL_TARGET: &str = "codexbar-aihubmix";
const ENV_KEYS: &[&str] = &[
    "AIHUBMIX_TOKEN",
    "AIHUBMIX_MANAGE_KEY",
    "AIHUBMIX_ACCESS_TOKEN",
];
const QUOTA_PER_USD: f64 = 500_000.0;

pub struct AiHubMixProvider {
    metadata: ProviderMetadata,
}

impl AiHubMixProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::AiHubMix,
                display_name: "AIHubMix",
                session_label: "Credit balance",
                weekly_label: "Account",
                supports_opus: false,
                supports_credits: true,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some(DASHBOARD_URL),
                status_page_url: None,
            },
        }
    }

    fn resolve_manage_key(api_key: Option<&str>) -> Result<String, ProviderError> {
        let raw = crate::providers::resolve_api_key(api_key, CREDENTIAL_TARGET, ENV_KEYS)?;
        let trimmed = raw.trim().trim_matches(['\'', '"']);
        let token = trimmed
            .strip_prefix("Bearer ")
            .or_else(|| trimmed.strip_prefix("bearer "))
            .unwrap_or(trimmed)
            .trim();
        if token.is_empty() {
            return Err(ProviderError::AuthRequired);
        }
        Ok(token.to_string())
    }
}

impl Default for AiHubMixProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct SelfResponse {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    data: Option<SelfData>,
}

#[derive(Debug, Deserialize)]
struct SelfData {
    #[serde(default)]
    quota: Option<serde_json::Value>,
    #[serde(default)]
    used_quota: Option<serde_json::Value>,
    #[serde(default)]
    group: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CreditValues {
    balance_usd: f64,
    used_usd: Option<f64>,
}

#[derive(Debug)]
struct AccountValues {
    credits: CreditValues,
    group: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RechargeBrowserCache {
    version: u32,
    provider: String,
    observed_at: i64,
    payload: RechargeBrowserPayload,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RechargeBrowserPayload {
    funded_balance_usd: f64,
    #[serde(default)]
    funding_created_at: Option<i64>,
    source: String,
}

fn parse_number(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
}

fn quota_to_usd(quota: f64, label: &str) -> Result<f64, ProviderError> {
    if !quota.is_finite() || quota < 0.0 {
        return Err(ProviderError::Parse(format!(
            "AIHubMix {label} was not a finite non-negative number"
        )));
    }
    Ok(quota / QUOTA_PER_USD)
}

impl SelfResponse {
    fn parse(text: &str) -> Result<AccountValues, ProviderError> {
        let parsed: Self = serde_json::from_str(text)
            .map_err(|error| ProviderError::Parse(format!("Invalid AIHubMix response: {error}")))?;
        if parsed.success == Some(false) {
            return Err(ProviderError::AuthRequired);
        }
        let data = parsed.data.ok_or_else(|| {
            ProviderError::Parse("AIHubMix response did not include data".to_string())
        })?;
        let quota = data.quota.as_ref().and_then(parse_number).ok_or_else(|| {
            ProviderError::Parse("AIHubMix response did not include quota".to_string())
        })?;
        let used_quota = data.used_quota.as_ref().and_then(parse_number);
        let credits = CreditValues {
            balance_usd: quota_to_usd(quota, "quota")?,
            used_usd: used_quota
                .map(|value| quota_to_usd(value, "used_quota"))
                .transpose()?,
        };
        let group = data
            .group
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Ok(AccountValues { credits, group })
    }
}

fn recharge_cache_path() -> Result<PathBuf, ProviderError> {
    dirs::data_local_dir()
        .map(|root| root.join("CodexBar").join(RECHARGE_CACHE_FILENAME))
        .ok_or_else(|| {
            ProviderError::NotInstalled(
                "Could not locate LOCALAPPDATA for AIHubMix recharge cache.".into(),
            )
        })
}

fn funded_balance_from_cache_raw(
    raw: &str,
    current_balance_usd: f64,
    now: DateTime<Utc>,
) -> Result<Option<f64>, ProviderError> {
    let cache: RechargeBrowserCache = serde_json::from_str(raw).map_err(|error| {
        ProviderError::Parse(format!("Invalid AIHubMix Browser Bridge cache: {error}"))
    })?;
    if cache.version != 1 || cache.provider != "aihubmix" {
        return Err(ProviderError::Parse(
            "Unsupported AIHubMix Browser Bridge cache version/provider".into(),
        ));
    }
    let observed_at = DateTime::<Utc>::from_timestamp(cache.observed_at, 0).ok_or_else(|| {
        ProviderError::Parse("Invalid AIHubMix Browser Bridge observation timestamp".into())
    })?;
    let age = (now - observed_at).num_seconds();
    if age < -CLOCK_SKEW_SECONDS {
        return Err(ProviderError::Other(
            "AIHubMix Browser Bridge snapshot is from the future; check the system clock.".into(),
        ));
    }
    if age > RECHARGE_CACHE_MAX_AGE_SECONDS {
        return Ok(None);
    }
    let payload = cache.payload;
    if !payload.funded_balance_usd.is_finite() || payload.funded_balance_usd <= 0.0 {
        return Err(ProviderError::Parse(
            "Invalid AIHubMix funded balance in Browser Bridge cache".into(),
        ));
    }
    if !matches!(payload.source.as_str(), "network" | "dom") {
        return Err(ProviderError::Parse(
            "Unknown AIHubMix Browser Bridge parser source".into(),
        ));
    }
    if payload
        .funding_created_at
        .is_some_and(|timestamp| timestamp <= 0 || timestamp > now.timestamp() + CLOCK_SKEW_SECONDS)
    {
        return Err(ProviderError::Parse(
            "Invalid AIHubMix funding timestamp in Browser Bridge cache".into(),
        ));
    }
    if payload.funded_balance_usd + 1e-9 < current_balance_usd {
        return Ok(None);
    }
    Ok(Some(payload.funded_balance_usd))
}

fn read_funded_balance_cache(
    current_balance_usd: f64,
    now: DateTime<Utc>,
) -> Result<Option<f64>, ProviderError> {
    let path = recharge_cache_path()?;
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ProviderError::Other(format!(
                "Failed to read AIHubMix Browser Bridge cache {}: {error}",
                path.display()
            )));
        }
    };
    funded_balance_from_cache_raw(&raw, current_balance_usd, now)
}

fn status_error(status: reqwest::StatusCode) -> ProviderError {
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        ProviderError::AuthRequired
    } else {
        ProviderError::Other(format!("AIHubMix API returned status {status}"))
    }
}

async fn fetch_self_url(
    client: &reqwest::Client,
    url: &str,
    manage_key: &str,
) -> Result<AccountValues, ProviderError> {
    async fn request(
        client: &reqwest::Client,
        url: &str,
        manage_key: &str,
        bearer: bool,
    ) -> Result<AccountValues, ProviderError> {
        let request = client
            .get(url)
            .header("Accept", "application/json")
            .header("Cache-Control", "no-store");
        let request = if bearer {
            request.bearer_auth(manage_key)
        } else {
            request.header("Authorization", manage_key)
        };
        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(status_error(response.status()));
        }
        let text = response.text().await.map_err(|error| {
            ProviderError::Other(format!("Failed to read AIHubMix response: {error}"))
        })?;
        SelfResponse::parse(&text)
    }

    // The current official AIHubMix CLI sends Manage Keys as a raw
    // Authorization value. Keep Bearer only as a compatibility fallback for
    // older access-token style configurations.
    match request(client, url, manage_key, false).await {
        Ok(result) => Ok(result),
        Err(ProviderError::AuthRequired) => request(client, url, manage_key, true).await,
        Err(error) => Err(error),
    }
}

async fn fetch_self(
    client: &reqwest::Client,
    manage_key: &str,
) -> Result<AccountValues, ProviderError> {
    match fetch_self_url(client, SELF_URL, manage_key).await {
        Ok(result) => Ok(result),
        Err(ProviderError::AuthRequired) => Err(ProviderError::AuthRequired),
        Err(primary_error) => {
            tracing::debug!(
                %primary_error,
                "AIHubMix primary platform endpoint unavailable; trying documented backup domain"
            );
            fetch_self_url(client, SELF_URL_FALLBACK, manage_key).await
        }
    }
}

fn result_from_values(
    values: CreditValues,
    group: Option<String>,
    funded_balance_usd: Option<f64>,
    source: &'static str,
) -> ProviderFetchResult {
    let (primary, used_usd, limit_usd) = match funded_balance_usd {
        Some(funded) if funded.is_finite() && funded > 0.0 && funded >= values.balance_usd => {
            let used = (funded - values.balance_usd).clamp(0.0, funded);
            let used_percent = (used / funded) * 100.0;
            (
                RateWindow::with_details(
                    used_percent,
                    None,
                    None,
                    Some(format!(
                        "{:.2} USD remaining of {:.2} USD funded",
                        values.balance_usd, funded
                    )),
                ),
                used,
                Some(funded),
            )
        }
        _ => (
            RateWindow::informational(format!("{:.2} USD credit balance", values.balance_usd)),
            0.0,
            None,
        ),
    };
    let mut snapshot = UsageSnapshot::new(primary);
    if let Some(group) = group {
        snapshot = snapshot.with_login_method(format!("Group {group}"));
    }
    let mut cost = CostSnapshot::new(used_usd, "USD", "Credits")
        .with_currency_symbol("$")
        .with_balance(values.balance_usd);
    if let Some(limit) = limit_usd {
        cost = cost.with_limit(limit);
    }
    ProviderFetchResult::new(snapshot, source).with_cost(cost)
}

async fn fetch_browser(timeout_secs: u64) -> Result<ProviderFetchResult, ProviderError> {
    let values = cdp::fetch_credit_values(timeout_secs).await?;
    Ok(result_from_values(values, None, None, "browser-cdp"))
}

#[async_trait]
impl Provider for AiHubMixProvider {
    fn id(&self) -> ProviderId {
        ProviderId::AiHubMix
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        match ctx.source_mode {
            SourceMode::Auto => {
                let manage_key = Self::resolve_manage_key(ctx.api_key.as_deref())?;
                let client = crate::core::credentialed_http_client_builder()
                    .timeout(std::time::Duration::from_secs(ctx.web_timeout.max(1)))
                    .build()
                    .map_err(|error| ProviderError::Other(error.to_string()))?;
                let account = fetch_self(&client, &manage_key).await?;
                let funded_balance_usd = match read_funded_balance_cache(
                    account.credits.balance_usd,
                    Utc::now(),
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::debug!(
                            %error,
                            "AIHubMix Browser Bridge recharge cache unavailable; keeping balance-only result"
                        );
                        None
                    }
                };
                let source = if funded_balance_usd.is_some() {
                    "api+browser-bridge"
                } else {
                    "api"
                };
                Ok(result_from_values(
                    account.credits,
                    account.group,
                    funded_balance_usd,
                    source,
                ))
            }
            SourceMode::Web => fetch_browser(ctx.web_timeout).await,
            SourceMode::OAuth | SourceMode::Cli => {
                Err(ProviderError::UnsupportedSource(ctx.source_mode))
            }
        }
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

    #[test]
    fn parses_documented_balance_units_without_retaining_identity() {
        let account = SelfResponse::parse(
            r#"{
                "success": true,
                "data": {
                    "email": "not-retained@example.com",
                    "access_token": "not-retained",
                    "quota": 29071257,
                    "used_quota": 286403484,
                    "group": "default"
                }
            }"#,
        )
        .unwrap();
        assert!((account.credits.balance_usd - 58.142514).abs() < 1e-9);
        assert!((account.credits.used_usd.unwrap() - 572.806968).abs() < 1e-9);
        assert_eq!(account.group.as_deref(), Some("default"));
    }

    #[test]
    fn accepts_numeric_strings_and_rejects_invalid_balance() {
        let account = SelfResponse::parse(
            r#"{"success":true,"data":{"quota":"500000","used_quota":"1000000"}}"#,
        )
        .unwrap();
        assert_eq!(account.credits.balance_usd, 1.0);
        assert_eq!(account.credits.used_usd, Some(2.0));

        assert!(
            SelfResponse::parse(r#"{"success":true,"data":{"quota":-1,"used_quota":0}}"#).is_err()
        );
        assert!(SelfResponse::parse(r#"{"success":true,"data":{}}"#).is_err());
    }

    #[test]
    fn unsuccessful_response_is_auth_required() {
        assert!(matches!(
            SelfResponse::parse(r#"{"success":false,"message":"unauthorized"}"#),
            Err(ProviderError::AuthRequired)
        ));
    }

    #[test]
    fn balance_result_uses_credits_style_cost_contract() {
        let result = result_from_values(
            CreditValues {
                balance_usd: 12.34,
                used_usd: Some(56.78),
            },
            Some("default".to_string()),
            None,
            "api",
        );
        assert!(result.usage.primary.is_informational);
        assert_eq!(
            result.usage.primary.reset_description.as_deref(),
            Some("12.34 USD credit balance")
        );
        assert_eq!(result.usage.login_method.as_deref(), Some("Group default"));
        let cost = result.cost.unwrap();
        assert_eq!(cost.used, 0.0);
        assert_eq!(cost.limit, None);
        assert_eq!(cost.balance, Some(12.34));
        assert_eq!(cost.period, "Credits");
    }

    #[test]
    fn recharge_anchor_produces_floatbar_percentage_and_cost_limit() {
        let result = result_from_values(
            CreditValues {
                balance_usd: 7.0,
                used_usd: Some(99.0),
            },
            Some("default".to_string()),
            Some(10.0),
            "api",
        );
        assert!(!result.usage.primary.is_informational);
        assert!((result.usage.primary.used_percent - 30.0).abs() < 1e-9);
        assert_eq!(
            result.usage.primary.reset_description.as_deref(),
            Some("7.00 USD remaining of 10.00 USD funded")
        );
        let cost = result.cost.unwrap();
        assert_eq!(cost.used, 3.0);
        assert_eq!(cost.limit, Some(10.0));
        assert_eq!(cost.balance, Some(7.0));
    }

    #[test]
    fn browser_recharge_cache_supplies_funded_balance() {
        let now = DateTime::parse_from_rfc3339("2026-09-04T03:40:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let raw = r#"{
            "version":1,
            "provider":"aihubmix",
            "observed_at":1788492600,
            "payload":{
                "funded_balance_usd":10.0,
                "funding_created_at":1788400000,
                "source":"network"
            }
        }"#;
        assert_eq!(
            funded_balance_from_cache_raw(raw, 7.0, now).unwrap(),
            Some(10.0)
        );
    }

    #[test]
    fn stale_or_smaller_browser_recharge_anchor_is_rejected() {
        let now = DateTime::parse_from_rfc3339("2026-09-04T03:40:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let smaller = r#"{"version":1,"provider":"aihubmix","observed_at":1788492600,"payload":{"funded_balance_usd":6.0,"funding_created_at":1788400000,"source":"network"}}"#;
        assert_eq!(
            funded_balance_from_cache_raw(smaller, 7.0, now).unwrap(),
            None
        );

        let stale = r#"{"version":1,"provider":"aihubmix","observed_at":1785000000,"payload":{"funded_balance_usd":10.0,"funding_created_at":1784000000,"source":"network"}}"#;
        assert_eq!(
            funded_balance_from_cache_raw(stale, 7.0, now).unwrap(),
            None
        );
    }

    #[test]
    fn metadata_exposes_topup_dashboard_and_browser_source() {
        let provider = AiHubMixProvider::new();
        assert_eq!(provider.id(), ProviderId::AiHubMix);
        assert_eq!(provider.metadata().dashboard_url, Some(DASHBOARD_URL));
        assert!(provider.metadata().supports_credits);
        assert_eq!(
            provider.available_sources(),
            vec![SourceMode::Auto, SourceMode::Web]
        );
    }

    #[tokio::test]
    async fn platform_request_uses_raw_manage_key_and_parses_balance() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/user/self")
            .match_header("authorization", "manage-test-token")
            .match_header("accept", "application/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"success":true,"data":{"quota":1250000,"used_quota":500000}}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let account = fetch_self_url(
            &client,
            &format!("{}/api/user/self", server.url()),
            "manage-test-token",
        )
        .await
        .unwrap();
        mock.assert_async().await;
        assert_eq!(account.credits.balance_usd, 2.5);
        assert_eq!(account.credits.used_usd, Some(1.0));
        assert_eq!(account.group, None);
    }

    #[test]
    fn browser_recharge_cache_rejects_secret_bearing_or_unknown_fields() {
        let now = DateTime::parse_from_rfc3339("2026-09-04T03:40:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let raw = r#"{"version":1,"provider":"aihubmix","observed_at":1788492600,"payload":{"funded_balance_usd":10.0,"funding_created_at":1788400000,"source":"network","access_token":"must-not-cross-boundary"}}"#;
        assert!(funded_balance_from_cache_raw(raw, 7.0, now).is_err());
    }
}
