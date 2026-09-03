//! AIHubMix account credit provider.
//!
//! AIHubMix exposes the current account balance through the documented
//! `GET /api/user/self` platform endpoint. The wire quota unit is 1 / 500000
//! USD, so the provider converts only the documented `quota` field and keeps
//! it as a prepaid balance (not a usage percentage).
//!
//! Auto mode prefers an explicitly configured AIHubMix Manage Key and falls
//! back to a signed-in local Chromium CDP session. The CDP path never extracts
//! browser credentials; it reads the already-authenticated `/api/user/self`
//! response body and emits only numeric quota fields to CodexBar.

mod cdp;

use async_trait::async_trait;
use serde::Deserialize;

use crate::core::{
    CostSnapshot, FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId,
    ProviderMetadata, RateWindow, SourceMode, UsageSnapshot,
};

const SELF_URL: &str = "https://aihubmix.com/api/user/self";
const SELF_URL_FALLBACK: &str = "https://api.aihubmix.com/api/user/self";
const DASHBOARD_URL: &str = "https://console.aihubmix.com/topup";
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
    fn parse(text: &str) -> Result<(CreditValues, Option<String>), ProviderError> {
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
        let values = CreditValues {
            balance_usd: quota_to_usd(quota, "quota")?,
            used_usd: used_quota
                .map(|value| quota_to_usd(value, "used_quota"))
                .transpose()?,
        };
        let group = data
            .group
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Ok((values, group))
    }
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
) -> Result<(CreditValues, Option<String>), ProviderError> {
    let response = client
        .get(url)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-store")
        .bearer_auth(manage_key)
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(status_error(response.status()));
    }
    let text = response.text().await.map_err(|error| {
        ProviderError::Other(format!("Failed to read AIHubMix response: {error}"))
    })?;
    SelfResponse::parse(&text)
}

async fn fetch_self(
    client: &reqwest::Client,
    manage_key: &str,
) -> Result<(CreditValues, Option<String>), ProviderError> {
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
    source: &'static str,
) -> ProviderFetchResult {
    let primary =
        RateWindow::informational(format!("{:.2} USD credit balance", values.balance_usd));
    let mut snapshot = UsageSnapshot::new(primary);
    if let Some(group) = group {
        snapshot = snapshot.with_login_method(format!("Group {group}"));
    }
    let cost = CostSnapshot::new(0.0, "USD", "Credits")
        .with_currency_symbol("$")
        .with_balance(values.balance_usd);
    ProviderFetchResult::new(snapshot, source).with_cost(cost)
}

async fn fetch_browser(timeout_secs: u64) -> Result<ProviderFetchResult, ProviderError> {
    let values = cdp::fetch_credit_values(timeout_secs).await?;
    Ok(result_from_values(values, None, "browser-cdp"))
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
                let (values, group) = fetch_self(&client, &manage_key).await?;
                Ok(result_from_values(values, group, "api"))
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
        let (values, group) = SelfResponse::parse(
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
        assert!((values.balance_usd - 58.142514).abs() < 1e-9);
        assert!((values.used_usd.unwrap() - 572.806968).abs() < 1e-9);
        assert_eq!(group.as_deref(), Some("default"));
    }

    #[test]
    fn accepts_numeric_strings_and_rejects_invalid_balance() {
        let (values, _) = SelfResponse::parse(
            r#"{"success":true,"data":{"quota":"500000","used_quota":"1000000"}}"#,
        )
        .unwrap();
        assert_eq!(values.balance_usd, 1.0);
        assert_eq!(values.used_usd, Some(2.0));

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
    async fn platform_request_uses_bearer_manage_key_and_parses_balance() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/user/self")
            .match_header("authorization", "Bearer manage-test-token")
            .match_header("accept", "application/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"success":true,"data":{"quota":1250000,"used_quota":500000}}"#)
            .create_async()
            .await;

        let client = reqwest::Client::new();
        let (values, group) = fetch_self_url(
            &client,
            &format!("{}/api/user/self", server.url()),
            "manage-test-token",
        )
        .await
        .unwrap();
        mock.assert_async().await;
        assert_eq!(values.balance_usd, 2.5);
        assert_eq!(values.used_usd, Some(1.0));
        assert_eq!(group, None);
    }
}
