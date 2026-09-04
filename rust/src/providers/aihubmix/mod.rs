//! AIHubMix account credit provider.
//!
//! AIHubMix exposes the current account balance through the documented
//! `GET /api/user/self` platform endpoint. The wire quota unit is 1 / 500000
//! USD. In API mode CodexBar also reads the current console transaction feed
//! and uses the newest active positive grant's `balance_after` as the funded
//! balance anchor. This lets the FloatBar show depletion since the latest
//! recharge (for example, $7 remaining from a $10 funded balance = 30% used).
//!
//! Auto mode uses an explicitly configured AIHubMix Manage Key and never
//! initiates CDP. Explicit Web mode can use a signed-in local Chromium CDP
//! session; that path never extracts browser credentials and emits only
//! sanitized numeric quota fields to CodexBar.

mod cdp;

use async_trait::async_trait;
use serde::Deserialize;

use crate::core::{
    CostSnapshot, FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId,
    ProviderMetadata, RateWindow, SourceMode, UsageSnapshot,
};

const SELF_URL: &str = "https://aihubmix.com/api/user/self";
const SELF_URL_FALLBACK: &str = "https://api.aihubmix.com/api/user/self";
const QUOTA_RECORD_URL: &str = "https://aihubmix.com/call/usr/quota_rec?p=0";
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
    id: Option<serde_json::Value>,
    #[serde(default)]
    access_token: Option<String>,
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

struct AccountValues {
    credits: CreditValues,
    group: Option<String>,
    user_id: Option<i64>,
    // Transient credential returned by /api/user/self. Never persisted,
    // rendered, or included in a Debug implementation.
    access_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QuotaRecordResponse {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    data: Vec<QuotaRecord>,
}

#[derive(Debug, Deserialize)]
struct QuotaRecord {
    #[serde(default)]
    grant_type: Option<serde_json::Value>,
    #[serde(default)]
    status: Option<serde_json::Value>,
    #[serde(default)]
    quota: Option<serde_json::Value>,
    #[serde(default)]
    balance_after: Option<serde_json::Value>,
    #[serde(default)]
    created_time: Option<serde_json::Value>,
}

fn parse_number(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
}

fn parse_i64(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(number) => number.as_i64(),
        serde_json::Value::String(text) => text.trim().parse::<i64>().ok(),
        _ => None,
    }
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
        let user_id = data.id.as_ref().and_then(parse_i64).filter(|id| *id > 0);
        let access_token = data
            .access_token
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        Ok(AccountValues {
            credits,
            group,
            user_id,
            access_token,
        })
    }
}

impl QuotaRecordResponse {
    fn parse(text: &str) -> Result<Self, ProviderError> {
        let parsed: Self = serde_json::from_str(text).map_err(|error| {
            ProviderError::Parse(format!("Invalid AIHubMix quota-record response: {error}"))
        })?;
        if parsed.success == Some(false) {
            return Err(ProviderError::AuthRequired);
        }
        Ok(parsed)
    }

    fn latest_funded_balance_usd(&self, current_balance_usd: f64) -> Option<f64> {
        let current_quota = current_balance_usd * QUOTA_PER_USD;
        self.data
            .iter()
            .filter_map(|record| {
                let grant_type = record.grant_type.as_ref().and_then(parse_i64)?;
                let status = record.status.as_ref().and_then(parse_i64)?;
                let quota = record.quota.as_ref().and_then(parse_number)?;
                let balance_after = record.balance_after.as_ref().and_then(parse_number)?;
                let created_time = record
                    .created_time
                    .as_ref()
                    .and_then(parse_i64)
                    .unwrap_or_default();

                // Console semantics: grant_type=4 is a deduction, status=1 is
                // the active/available state, and positive quota rows are
                // funding events (top-up, exchange, admin grant, auto top-up).
                if grant_type == 4
                    || status != 1
                    || !quota.is_finite()
                    || quota <= 0.0
                    || !balance_after.is_finite()
                    || balance_after <= 0.0
                    || balance_after + 0.5 < current_quota
                {
                    return None;
                }
                Some((created_time, balance_after))
            })
            .max_by_key(|(created_time, _)| *created_time)
            .and_then(|(_, balance_after)| quota_to_usd(balance_after, "balance_after").ok())
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

async fn fetch_quota_records_with_token(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    user_id: i64,
    bearer: bool,
) -> Result<QuotaRecordResponse, ProviderError> {
    let request = client
        .get(url)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-store")
        .header("New-Api-User", user_id.to_string());
    let request = if bearer {
        request.bearer_auth(token)
    } else {
        request.header("Authorization", token)
    };
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(status_error(response.status()));
    }
    let text = response.text().await.map_err(|error| {
        ProviderError::Other(format!("Failed to read AIHubMix quota records: {error}"))
    })?;
    QuotaRecordResponse::parse(&text)
}

async fn fetch_funded_balance(
    client: &reqwest::Client,
    manage_key: &str,
    access_token: Option<&str>,
    user_id: Option<i64>,
    current_balance_usd: f64,
) -> Result<Option<f64>, ProviderError> {
    let Some(user_id) = user_id else {
        return Ok(None);
    };

    // First try the official Manage Key form: raw Authorization value.
    match fetch_quota_records_with_token(client, QUOTA_RECORD_URL, manage_key, user_id, false).await
    {
        Ok(records) => return Ok(records.latest_funded_balance_usd(current_balance_usd)),
        Err(ProviderError::AuthRequired) => {}
        Err(error) => return Err(error),
    }

    // Retain Bearer compatibility for older system/access-token setups.
    match fetch_quota_records_with_token(client, QUOTA_RECORD_URL, manage_key, user_id, true).await
    {
        Ok(records) => return Ok(records.latest_funded_balance_usd(current_balance_usd)),
        Err(ProviderError::AuthRequired) => {}
        Err(error) => return Err(error),
    }

    // /api/user/self already returns access_token. Reuse that value in-memory
    // for the user-scoped Transactions route. Do not call /api/user/token:
    // compatible backends may implement that GET as token regeneration.
    let Some(access_token) = access_token else {
        return Ok(None);
    };
    match fetch_quota_records_with_token(client, QUOTA_RECORD_URL, access_token, user_id, false)
        .await
    {
        Ok(records) => Ok(records.latest_funded_balance_usd(current_balance_usd)),
        Err(ProviderError::AuthRequired) => {
            let records = fetch_quota_records_with_token(
                client,
                QUOTA_RECORD_URL,
                access_token,
                user_id,
                true,
            )
            .await?;
            Ok(records.latest_funded_balance_usd(current_balance_usd))
        }
        Err(error) => Err(error),
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
                tracing::debug!(
                    user_id_present = account.user_id.is_some(),
                    "AIHubMix account metadata resolved for recharge-history enrichment"
                );
                let funded_balance_usd = match fetch_funded_balance(
                    &client,
                    &manage_key,
                    account.access_token.as_deref(),
                    account.user_id,
                    account.credits.balance_usd,
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::debug!(
                            %error,
                            "AIHubMix recharge history unavailable; keeping balance-only result"
                        );
                        None
                    }
                };
                Ok(result_from_values(
                    account.credits,
                    account.group,
                    funded_balance_usd,
                    "api",
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
        assert_eq!(account.access_token.as_deref(), Some("not-retained"));
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
    fn quota_records_pick_latest_active_positive_grant_balance_after() {
        let records = QuotaRecordResponse::parse(
            r#"{
                "success": true,
                "data": [
                    {"grant_type":4,"status":1,"quota":-500000,"balance_after":3500000,"created_time":30},
                    {"grant_type":2,"status":2,"quota":2000000,"balance_after":5500000,"created_time":25},
                    {"grant_type":1,"status":1,"quota":2000000,"balance_after":5000000,"created_time":20},
                    {"grant_type":3,"status":1,"quota":500000,"balance_after":3000000,"created_time":10}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(records.latest_funded_balance_usd(7.0), Some(10.0));
    }

    #[test]
    fn stale_or_smaller_recharge_anchor_is_rejected() {
        let records = QuotaRecordResponse::parse(
            r#"{"success":true,"data":[{"grant_type":1,"status":1,"quota":500000,"balance_after":3000000,"created_time":10}]}"#,
        )
        .unwrap();
        assert_eq!(records.latest_funded_balance_usd(7.0), None);
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

    #[tokio::test]
    async fn quota_record_request_supports_raw_user_access_token_without_regeneration() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/call/usr/quota_rec")
            .match_query(mockito::Matcher::UrlEncoded("p".into(), "0".into()))
            .match_header("authorization", "user-access-token")
            .match_header("new-api-user", "123")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"success":true,"data":[{"grant_type":1,"status":1,"quota":5000000,"balance_after":5000000,"created_time":10}]}"#,
            )
            .create_async()
            .await;

        let records = fetch_quota_records_with_token(
            &reqwest::Client::new(),
            &format!("{}/call/usr/quota_rec?p=0", server.url()),
            "user-access-token",
            123,
            false,
        )
        .await
        .unwrap();
        mock.assert_async().await;
        assert_eq!(records.latest_funded_balance_usd(7.0), Some(10.0));
    }
}
