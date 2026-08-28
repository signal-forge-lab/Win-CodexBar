//! OrcaRouter provider implementation
//!
//! Fetches workspace-level usage/subscription summaries from the OrcaRouter
//! billing API. Both endpoints are workspace summaries (never per-key spend):
//!
//! - `GET /v1/dashboard/billing/usage` → `{ object, total_usage }`
//! - `GET /v1/dashboard/billing/subscription` →
//!   `{ object, has_payment_method, soft_limit_usd, hard_limit_usd,
//!      system_hard_limit_usd, access_until }`
//!
//! `total_usage` follows the legacy OpenAI billing shape and is reported in
//! hundredths of a USD, while the `*_limit_usd` fields are USD amounts.
//! Historical or per-request billing is intentionally not implemented; the
//! endpoints above are summary-only.

mod cdp;
mod local_storage;

use async_trait::async_trait;
use chrono::DateTime;
use serde::Deserialize;

use crate::core::{
    CostSnapshot, FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId,
    ProviderMetadata, RateWindow, SourceMode, UsageSnapshot,
};

const ORCA_API_BASE: &str = "https://api.orcarouter.ai/v1";
const USAGE_URL: &str = "https://api.orcarouter.ai/v1/dashboard/billing/usage";
const SUBSCRIPTION_URL: &str = "https://api.orcarouter.ai/v1/dashboard/billing/subscription";
const WEB_SELF_URL: &str = "https://www.orcarouter.ai/api/user/self";
const WEB_STATUS_URL: &str = "https://www.orcarouter.ai/api/status";
const WEB_COOKIE_DOMAINS: &[&str] = &["www.orcarouter.ai", "orcarouter.ai"];
const CREDENTIAL_TARGET: &str = "codexbar-orcarouter";
const ENV_KEYS: &[&str] = &["ORCAROUTER_API_KEY"];
const ORCA_DASHBOARD_URL: &str = "https://www.orcarouter.ai/console";

/// Limits at or above this USD amount are treated as an effectively uncapped
/// spend ceiling. Rendering a quota percent against a 10^6 sentinel would show
/// a misleading 0% bar for real spend.
const NO_CAP_SENTINEL_USD: f64 = 1_000_000.0;

pub struct OrcaRouterProvider {
    metadata: ProviderMetadata,
}

impl OrcaRouterProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::OrcaRouter,
                display_name: "OrcaRouter",
                session_label: "Workspace usage",
                weekly_label: "Spend limit",
                supports_opus: false,
                supports_credits: true,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some(ORCA_DASHBOARD_URL),
                status_page_url: None,
            },
        }
    }

    fn resolve_api_key(api_key: Option<&str>) -> Result<String, ProviderError> {
        crate::providers::resolve_api_key(api_key, CREDENTIAL_TARGET, ENV_KEYS)
    }

    fn resolve_web_cookie(ctx: &FetchContext) -> Result<String, ProviderError> {
        if let Some(cookie) = ctx.manual_cookie_header.as_deref() {
            let cookie = cookie.trim();
            if !cookie.is_empty() {
                return Ok(cookie.to_string());
            }
        }
        // Manual/token-scoped settings must not silently mix in an ambient
        // browser account. `auto_prefer_web` is the existing shell signal for
        // those explicitly scoped credentials.
        if ctx.auto_prefer_web {
            return Err(ProviderError::NoCookies);
        }
        crate::providers::browser_cookie_header(WEB_COOKIE_DOMAINS)
    }
}

impl Default for OrcaRouterProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    #[serde(default)]
    total_usage: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct WebSelfResponse {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    data: Option<WebSelfData>,
}

#[derive(Debug, Deserialize)]
struct WebSelfData {
    #[serde(default)]
    active_workspace: Option<WebWorkspace>,
}

#[derive(Debug, Deserialize)]
struct WebWorkspace {
    #[serde(default)]
    wallet_quota: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    data: Option<StatusData>,
}

#[derive(Debug, Deserialize)]
struct StatusData {
    #[serde(default)]
    quota_per_unit: Option<f64>,
}

/// Subscription summary; every field is optional so partial responses parse.
#[derive(Debug, Deserialize)]
pub struct SubscriptionSummary {
    #[serde(default)]
    pub has_payment_method: Option<bool>,
    #[serde(default)]
    pub soft_limit_usd: Option<f64>,
    #[serde(default)]
    pub hard_limit_usd: Option<f64>,
    #[serde(default)]
    pub system_hard_limit_usd: Option<f64>,
    /// Unix seconds when workspace access expires; 0 means no expiry. `None`
    /// when absent or explicitly null, so partial responses always parse.
    #[serde(default)]
    pub access_until: Option<i64>,
}

fn parse_json<T: serde::de::DeserializeOwned>(text: &str, label: &str) -> Result<T, ProviderError> {
    serde_json::from_str(text)
        .map_err(|e| ProviderError::Parse(format!("Invalid OrcaRouter {label} response: {e}")))
}

impl UsageResponse {
    fn parse(text: &str) -> Result<Self, ProviderError> {
        let parsed: UsageResponse = parse_json(text, "usage")?;
        if parsed.total_usage.is_none() {
            // Never synthesize 0 from absent/null data.
            return Err(ProviderError::Parse(
                "OrcaRouter usage response did not include total_usage".to_string(),
            ));
        }
        Ok(parsed)
    }
}

impl WebSelfResponse {
    fn parse(text: &str) -> Result<Self, ProviderError> {
        let parsed: Self = parse_json(text, "web account")?;
        if parsed.success == Some(false) {
            return Err(ProviderError::AuthRequired);
        }
        parsed.wallet_quota()?;
        Ok(parsed)
    }

    fn wallet_quota(&self) -> Result<f64, ProviderError> {
        let quota = self
            .data
            .as_ref()
            .and_then(|data| data.active_workspace.as_ref())
            .and_then(|workspace| workspace.wallet_quota)
            .ok_or_else(|| {
                ProviderError::Parse(
                    "OrcaRouter web account response did not include active_workspace.wallet_quota"
                        .to_string(),
                )
            })?;
        if !quota.is_finite() || quota < 0.0 {
            return Err(ProviderError::Parse(
                "OrcaRouter wallet_quota was not a finite non-negative number".to_string(),
            ));
        }
        Ok(quota)
    }
}

impl StatusResponse {
    fn parse(text: &str) -> Result<Self, ProviderError> {
        let parsed: Self = parse_json(text, "status")?;
        if parsed.success == Some(false) || parsed.quota_per_unit().is_none() {
            return Err(ProviderError::Parse(
                "OrcaRouter status response did not include a usable quota_per_unit".to_string(),
            ));
        }
        Ok(parsed)
    }

    fn quota_per_unit(&self) -> Option<f64> {
        self.data
            .as_ref()
            .and_then(|data| data.quota_per_unit)
            .filter(|rate| rate.is_finite() && *rate > 0.0)
    }
}

fn wallet_balance_usd(wallet_quota: f64, status: &StatusResponse) -> Result<f64, ProviderError> {
    let quota_per_unit = status.quota_per_unit().ok_or_else(|| {
        ProviderError::Parse("OrcaRouter quota_per_unit is unavailable".to_string())
    })?;
    if !wallet_quota.is_finite() || wallet_quota < 0.0 {
        return Err(ProviderError::Parse(
            "OrcaRouter wallet_quota was not a finite non-negative number".to_string(),
        ));
    }
    Ok(wallet_quota / quota_per_unit)
}

impl SubscriptionSummary {
    fn parse(text: &str) -> Result<Self, ProviderError> {
        parse_json(text, "subscription")
    }

    /// Effective spend cap in USD: the smallest positive reported limit.
    /// `None` when the API reports no usable limit (absent, null, or zero).
    fn effective_cap_usd(&self) -> Option<f64> {
        [self.hard_limit_usd, self.system_hard_limit_usd]
            .into_iter()
            .flatten()
            .filter(|v| v.is_finite() && *v > 0.0)
            .fold(None, |min: Option<f64>, v| match min {
                Some(m) => Some(m.min(v)),
                None => Some(v),
            })
    }
}

/// Map a non-success HTTP status to the provider error family used by tests
/// and callers. Returns `None` for success statuses.
fn status_error(status: reqwest::StatusCode) -> Option<ProviderError> {
    if status.is_success() {
        return None;
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        Some(ProviderError::AuthRequired)
    } else {
        Some(ProviderError::Other(format!(
            "OrcaRouter API returned status {}",
            status
        )))
    }
}

async fn get_text(client: &reqwest::Client, url: &str, key: &str) -> Result<String, ProviderError> {
    let resp = client
        .get(url)
        .bearer_auth(key)
        .header("Accept", "application/json")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(status_error(resp.status()).expect("non-success statuses map to an error"));
    }
    resp.text()
        .await
        .map_err(|e| ProviderError::Other(format!("Failed to read OrcaRouter response: {e}")))
}

async fn get_web_text(
    client: &reqwest::Client,
    url: &str,
    cookie_header: Option<&str>,
) -> Result<String, ProviderError> {
    let mut request = client.get(url).header("Accept", "application/json");
    if let Some(cookie) = cookie_header {
        request = request.header(reqwest::header::COOKIE, cookie);
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(status_error(response.status()).expect("non-success statuses map to an error"));
    }
    response.text().await.map_err(|error| {
        ProviderError::Other(format!("Failed to read OrcaRouter web response: {error}"))
    })
}

async fn get_web_text_for_identity(
    client: &reqwest::Client,
    url: &str,
    identity: &local_storage::BrowserIdentity,
) -> Result<String, ProviderError> {
    let mut request = client
        .get(url)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-store")
        .header("New-API-User", &identity.user_id);
    if let Some(workspace_id) = identity.workspace_id.as_deref() {
        request = request.header("X-Workspace-Id", workspace_id);
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(status_error(response.status()).expect("non-success statuses map to an error"));
    }
    response.text().await.map_err(|error| {
        ProviderError::Other(format!("Failed to read OrcaRouter web response: {error}"))
    })
}

async fn get_web_text_for_session(
    client: &reqwest::Client,
    url: &str,
    cookie_header: &str,
    identity: &local_storage::BrowserIdentity,
) -> Result<String, ProviderError> {
    let mut request = client
        .get(url)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-store")
        .header(reqwest::header::COOKIE, cookie_header)
        .header("New-API-User", &identity.user_id);
    if let Some(workspace_id) = identity.workspace_id.as_deref() {
        request = request.header("X-Workspace-Id", workspace_id);
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(status_error(response.status()).expect("non-success statuses map to an error"));
    }
    response.text().await.map_err(|error| {
        ProviderError::Other(format!("Failed to read OrcaRouter web response: {error}"))
    })
}

async fn fetch_wallet_balance(
    client: &reqwest::Client,
    self_url: &str,
    status_url: &str,
    cookie_header: &str,
) -> Result<f64, ProviderError> {
    let (account_text, status_text) = tokio::join!(
        get_web_text(client, self_url, Some(cookie_header)),
        get_web_text(client, status_url, None),
    );
    let account = WebSelfResponse::parse(&account_text?)?;
    let status = StatusResponse::parse(&status_text?)?;
    wallet_balance_usd(account.wallet_quota()?, &status)
}

async fn fetch_wallet_balance_for_identity(
    client: &reqwest::Client,
    self_url: &str,
    status_url: &str,
    identity: &local_storage::BrowserIdentity,
) -> Result<f64, ProviderError> {
    let (account_text, status_text) = tokio::join!(
        get_web_text_for_identity(client, self_url, identity),
        get_web_text(client, status_url, None),
    );
    let account = WebSelfResponse::parse(&account_text?)?;
    let status = StatusResponse::parse(&status_text?)?;
    wallet_balance_usd(account.wallet_quota()?, &status)
}

async fn fetch_wallet_balance_for_session(
    client: &reqwest::Client,
    self_url: &str,
    status_url: &str,
    cookie_header: &str,
    identity: &local_storage::BrowserIdentity,
) -> Result<f64, ProviderError> {
    let (account_text, status_text) = tokio::join!(
        get_web_text_for_session(client, self_url, cookie_header, identity),
        get_web_text(client, status_url, None),
    );
    let account = WebSelfResponse::parse(&account_text?)?;
    let status = StatusResponse::parse(&status_text?)?;
    wallet_balance_usd(account.wallet_quota()?, &status)
}

async fn fetch_status(
    client: &reqwest::Client,
    status_url: &str,
) -> Result<StatusResponse, ProviderError> {
    let text = get_web_text(client, status_url, None).await?;
    StatusResponse::parse(&text)
}

async fn fetch_browser_wallet_balance(
    client: &reqwest::Client,
    ctx: &FetchContext,
) -> Result<f64, ProviderError> {
    // Explicit manual/token-scoped sessions must never be replaced by a different
    // ambient browser account. Only automatic browser mode may use LocalStorage
    // identity or CDP discovery.
    if !ctx.auto_prefer_web {
        for identity in local_storage::browser_identities() {
            match fetch_wallet_balance_for_identity(client, WEB_SELF_URL, WEB_STATUS_URL, &identity)
                .await
            {
                Ok(balance) => return Ok(balance),
                Err(error) => tracing::debug!(
                    %error,
                    "OrcaRouter LocalStorage identity did not authenticate; trying another browser session"
                ),
            }
        }

        if let Ok(wallet_quota) = cdp::fetch_wallet_quota(ctx.web_timeout).await {
            let status = fetch_status(client, WEB_STATUS_URL).await?;
            return wallet_balance_usd(wallet_quota, &status);
        }
    }

    let cookie = match OrcaRouterProvider::resolve_web_cookie(ctx) {
        Ok(cookie) => cookie,
        Err(error) if !ctx.auto_prefer_web => {
            tracing::debug!(
                %error,
                "OrcaRouter automatic browser session was not authenticated"
            );
            return Err(ProviderError::Other(
                "No authenticated OrcaRouter browser session found. Sign in to www.orcarouter.ai in Edge/Chrome, then refresh CodexBar."
                    .to_string(),
            ));
        }
        Err(error) => return Err(error),
    };
    for identity in local_storage::browser_identities() {
        match fetch_wallet_balance_for_session(
            client,
            WEB_SELF_URL,
            WEB_STATUS_URL,
            &cookie,
            &identity,
        )
        .await
        {
            Ok(balance) => return Ok(balance),
            Err(error) => tracing::debug!(
                %error,
                "OrcaRouter saved cookie did not match this browser identity; trying another"
            ),
        }
    }
    fetch_wallet_balance(client, WEB_SELF_URL, WEB_STATUS_URL, &cookie).await
}

/// Fetch and parse both workspace summaries. The subscription call degrades
/// gracefully (usage still renders without limits). Split out so tests can
/// drive it against a local mock server.
async fn fetch_summaries(
    client: &reqwest::Client,
    usage_url: &str,
    subscription_url: &str,
    api_key: &str,
) -> Result<(UsageResponse, Option<SubscriptionSummary>), ProviderError> {
    let (usage_res, sub_res) = tokio::join!(
        get_text(client, usage_url, api_key),
        get_text(client, subscription_url, api_key),
    );

    let usage = UsageResponse::parse(&usage_res?)?;
    // Either leg failure below (HTTP error or unreadable body) degrades to
    // usage-without-limits; a partial subscription must never take down the
    // already-fetched usage row.
    let subscription = sub_res
        .and_then(|text| SubscriptionSummary::parse(&text))
        .map_err(|error| {
            tracing::warn!(
                %error,
                "OrcaRouter subscription degraded; showing usage without limits"
            );
        })
        .ok();
    Ok((usage, subscription))
}

/// Build the workspace snapshot from the OpenAI-shape `total_usage` value
/// (hundredths of a USD) plus optional subscription summary.
fn workspace_snapshot(total_usage_cents: f64, sub: Option<&SubscriptionSummary>) -> UsageSnapshot {
    let total_usage_usd = (total_usage_cents / 100.0).max(0.0);
    let cap = sub.and_then(|s| s.effective_cap_usd());
    let primary = match cap {
        Some(cap) if cap < NO_CAP_SENTINEL_USD => {
            let mut window = RateWindow::new(((total_usage_usd / cap) * 100.0).clamp(0.0, 100.0));
            window.reset_description = Some(format!(
                "{:.2} of {:.2} USD workspace usage",
                total_usage_usd, cap
            ));
            window
        }
        _ => {
            // No usable cap: surface the real total without inventing a percent.
            RateWindow::informational(format!("{:.2} USD workspace usage", total_usage_usd))
        }
    };

    let mut notes = Vec::new();
    if let Some(sub) = sub {
        if sub.has_payment_method == Some(true) {
            notes.push("payment method on file".to_string());
        }
        if let Some(until) = sub.access_until
            && until > 0
            && let Some(expires) = DateTime::from_timestamp(until, 0)
        {
            notes.push(format!("access until {}", expires.format("%Y-%m-%d")));
        }
    }
    let login_method = if notes.is_empty() {
        None
    } else {
        Some(notes.join(" · "))
    };

    let snapshot = UsageSnapshot::new(primary);
    match login_method {
        Some(method) => snapshot.with_login_method(method),
        None => snapshot,
    }
}

fn workspace_cost(
    total_usage_cents: f64,
    sub: Option<&SubscriptionSummary>,
    wallet_balance: Option<f64>,
) -> CostSnapshot {
    let used_usd = (total_usage_cents / 100.0).max(0.0);
    let mut cost = CostSnapshot::new(used_usd, "USD", "Workspace").with_currency_symbol("$");
    if let Some(cap) = sub.and_then(SubscriptionSummary::effective_cap_usd)
        && cap < NO_CAP_SENTINEL_USD
    {
        cost = cost.with_limit(cap);
    }
    if let Some(balance) = wallet_balance {
        cost = cost.with_balance(balance);
    }
    cost
}

fn wallet_snapshot(balance_usd: f64) -> UsageSnapshot {
    UsageSnapshot::new(RateWindow::informational(format!(
        "{balance_usd:.2} USD wallet balance"
    )))
}

fn wallet_cost(balance_usd: f64) -> CostSnapshot {
    CostSnapshot::new(0.0, "USD", "Wallet")
        .with_currency_symbol("$")
        .with_balance(balance_usd)
}

fn merge_auto_results(
    api_result: Result<(UsageResponse, Option<SubscriptionSummary>), ProviderError>,
    wallet_result: Option<Result<f64, ProviderError>>,
) -> Result<ProviderFetchResult, ProviderError> {
    match api_result {
        Ok((usage, subscription)) => {
            let wallet_balance = match wallet_result {
                Some(Ok(balance)) => Some(balance),
                Some(Err(error)) => {
                    tracing::debug!(
                        %error,
                        "OrcaRouter browser wallet enrichment unavailable; keeping API usage"
                    );
                    None
                }
                None => None,
            };
            let snapshot =
                workspace_snapshot(usage.total_usage.unwrap_or_default(), subscription.as_ref());
            let cost = workspace_cost(
                usage.total_usage.unwrap_or_default(),
                subscription.as_ref(),
                wallet_balance,
            );
            let source = if wallet_balance.is_some() {
                "api+web"
            } else {
                "api"
            };
            Ok(ProviderFetchResult::new(snapshot, source).with_cost(cost))
        }
        Err(api_error) => match wallet_result {
            Some(Ok(balance)) => {
                tracing::debug!(
                    %api_error,
                    "OrcaRouter API summary unavailable; falling back to browser wallet"
                );
                Ok(ProviderFetchResult::new(wallet_snapshot(balance), "web")
                    .with_cost(wallet_cost(balance)))
            }
            Some(Err(web_error)) => {
                tracing::debug!(
                    %web_error,
                    "OrcaRouter browser wallet fallback unavailable"
                );
                Err(api_error)
            }
            None => Err(api_error),
        },
    }
}

#[async_trait]
impl Provider for OrcaRouterProvider {
    fn id(&self) -> ProviderId {
        ProviderId::OrcaRouter
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        match ctx.source_mode {
            SourceMode::Auto => {
                let client = crate::core::credentialed_http_client_builder()
                    .timeout(std::time::Duration::from_secs(ctx.web_timeout.max(1)))
                    .build()
                    .map_err(|e| ProviderError::Other(e.to_string()))?;

                tracing::debug!(
                    "Fetching OrcaRouter workspace summary with optional wallet enrichment"
                );

                let api_result = match Self::resolve_api_key(ctx.api_key.as_deref()) {
                    Ok(api_key) => {
                        fetch_summaries(&client, USAGE_URL, SUBSCRIPTION_URL, &api_key).await
                    }
                    Err(error) => Err(error),
                };
                let wallet_result = if ctx.include_credits {
                    Some(fetch_browser_wallet_balance(&client, ctx).await)
                } else {
                    None
                };

                merge_auto_results(api_result, wallet_result)
            }
            SourceMode::OAuth => {
                let api_key = Self::resolve_api_key(ctx.api_key.as_deref())?;
                let client = crate::core::credentialed_http_client_builder()
                    .timeout(std::time::Duration::from_secs(ctx.web_timeout.max(1)))
                    .build()
                    .map_err(|e| ProviderError::Other(e.to_string()))?;
                let (usage, subscription) =
                    fetch_summaries(&client, USAGE_URL, SUBSCRIPTION_URL, &api_key).await?;
                let snapshot = workspace_snapshot(
                    usage.total_usage.unwrap_or_default(),
                    subscription.as_ref(),
                );
                let cost = workspace_cost(
                    usage.total_usage.unwrap_or_default(),
                    subscription.as_ref(),
                    None,
                );
                Ok(ProviderFetchResult::new(snapshot, "api").with_cost(cost))
            }
            SourceMode::Web => {
                let client = crate::core::credentialed_http_client_builder()
                    .timeout(std::time::Duration::from_secs(ctx.web_timeout.max(1)))
                    .build()
                    .map_err(|e| ProviderError::Other(e.to_string()))?;
                let balance = fetch_browser_wallet_balance(&client, ctx).await?;
                Ok(ProviderFetchResult::new(wallet_snapshot(balance), "web")
                    .with_cost(wallet_cost(balance)))
            }
            SourceMode::Cli => Err(ProviderError::UnsupportedSource(ctx.source_mode)),
        }
    }

    fn available_sources(&self) -> Vec<SourceMode> {
        vec![SourceMode::Auto, SourceMode::OAuth, SourceMode::Web]
    }

    fn supports_web(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPLETE_USAGE: &str = r#"{"object":"list","total_usage":4574.52}"#;
    const COMPLETE_SUB: &str = r#"{
        "object": "billing_subscription",
        "has_payment_method": true,
        "soft_limit_usd": 60.0,
        "hard_limit_usd": 120.0,
        "system_hard_limit_usd": 100.0,
        "access_until": 1735689600
    }"#;

    const COMPLETE_WEB_SELF: &str = r#"{
        "success": true,
        "data": {
            "active_workspace": {
                "wallet_quota": 2505000
            }
        }
    }"#;

    const COMPLETE_STATUS: &str = r#"{
        "success": true,
        "data": {
            "quota_per_unit": 500000,
            "display_in_currency": true,
            "quota_display_type": "USD"
        }
    }"#;

    #[test]
    fn parses_browser_wallet_and_runtime_quota_rate() {
        let account = WebSelfResponse::parse(COMPLETE_WEB_SELF).unwrap();
        let status = StatusResponse::parse(COMPLETE_STATUS).unwrap();

        assert_eq!(account.wallet_quota().unwrap(), 2_505_000.0);
        assert_eq!(status.quota_per_unit(), Some(500_000.0));
        assert_eq!(
            wallet_balance_usd(account.wallet_quota().unwrap(), &status).unwrap(),
            5.01
        );
    }

    #[test]
    fn combined_cost_keeps_workspace_spend_and_wallet_balance_separate() {
        let sub = SubscriptionSummary::parse(COMPLETE_SUB).unwrap();
        let cost = workspace_cost(1499.4496, Some(&sub), Some(5.01));

        assert!((cost.used - 14.994496).abs() < 1e-9);
        assert_eq!(cost.limit, Some(100.0));
        assert_eq!(cost.balance, Some(5.01));
        assert_eq!(cost.currency_code, "USD");
        assert_eq!(cost.currency_symbol.as_deref(), Some("$"));
    }

    #[tokio::test]
    async fn browser_wallet_fetch_uses_cookie_session_and_public_status_rate() {
        let mut server = mockito::Server::new_async().await;
        let self_mock = server
            .mock("GET", "/api/user/self")
            .match_header("cookie", "session=browser-session")
            .with_status(200)
            .with_body(COMPLETE_WEB_SELF)
            .create_async()
            .await;
        let status_mock = server
            .mock("GET", "/api/status")
            .with_status(200)
            .with_body(COMPLETE_STATUS)
            .create_async()
            .await;

        let balance = fetch_wallet_balance(
            &reqwest::Client::new(),
            &format!("{}/api/user/self", server.url()),
            &format!("{}/api/status", server.url()),
            "session=browser-session",
        )
        .await
        .unwrap();

        self_mock.assert_async().await;
        status_mock.assert_async().await;
        assert_eq!(balance, 5.01);
    }

    #[tokio::test]
    async fn browser_wallet_fetch_uses_cookie_and_browser_identity_headers() {
        let mut server = mockito::Server::new_async().await;
        let self_mock = server
            .mock("GET", "/api/user/self")
            .match_header("cookie", "session=browser-session")
            .match_header("new-api-user", "19915")
            .match_header("x-workspace-id", "19645")
            .with_status(200)
            .with_body(COMPLETE_WEB_SELF)
            .create_async()
            .await;
        let status_mock = server
            .mock("GET", "/api/status")
            .with_status(200)
            .with_body(COMPLETE_STATUS)
            .create_async()
            .await;
        let identity = local_storage::BrowserIdentity {
            user_id: "19915".to_string(),
            workspace_id: Some("19645".to_string()),
        };

        let balance = fetch_wallet_balance_for_session(
            &reqwest::Client::new(),
            &format!("{}/api/user/self", server.url()),
            &format!("{}/api/status", server.url()),
            "session=browser-session",
            &identity,
        )
        .await
        .unwrap();

        self_mock.assert_async().await;
        status_mock.assert_async().await;
        assert_eq!(balance, 5.01);
    }

    #[test]
    fn provider_exposes_browser_source_for_wallet_balance() {
        let provider = OrcaRouterProvider::new();
        assert!(provider.metadata().supports_credits);
        assert!(provider.supports_web());
        assert_eq!(
            provider.available_sources(),
            vec![SourceMode::Auto, SourceMode::OAuth, SourceMode::Web]
        );
    }

    #[test]
    fn manual_or_token_scoped_cookie_mode_never_falls_back_to_ambient_browser_session() {
        let ctx = FetchContext {
            auto_prefer_web: true,
            ..FetchContext::default()
        };
        assert!(matches!(
            OrcaRouterProvider::resolve_web_cookie(&ctx),
            Err(ProviderError::NoCookies)
        ));
    }

    #[test]
    fn auto_merge_falls_back_to_wallet_when_api_is_unavailable() {
        let result = merge_auto_results(Err(ProviderError::AuthRequired), Some(Ok(5.01))).unwrap();
        assert_eq!(result.source_label, "web");
        assert_eq!(
            result.cost.as_ref().and_then(|cost| cost.balance),
            Some(5.01)
        );
        assert_eq!(
            result.usage.primary.reset_description.as_deref(),
            Some("5.01 USD wallet balance")
        );
    }

    #[test]
    fn auto_merge_keeps_api_usage_when_wallet_enrichment_fails() {
        let usage = UsageResponse::parse(COMPLETE_USAGE).unwrap();
        let subscription = SubscriptionSummary::parse(COMPLETE_SUB).unwrap();
        let result = merge_auto_results(
            Ok((usage, Some(subscription))),
            Some(Err(ProviderError::NoCookies)),
        )
        .unwrap();
        assert_eq!(result.source_label, "api");
        assert!(
            result
                .cost
                .as_ref()
                .is_some_and(|cost| cost.balance.is_none())
        );
    }

    #[test]
    fn parses_complete_workspace_summary() {
        let usage = UsageResponse::parse(COMPLETE_USAGE).unwrap();
        assert!((usage.total_usage.unwrap() - 4574.52).abs() < 1e-9);

        let sub = SubscriptionSummary::parse(COMPLETE_SUB).unwrap();
        assert_eq!(sub.has_payment_method, Some(true));
        // Effective cap is the smallest positive reported limit.
        assert_eq!(sub.effective_cap_usd(), Some(100.0));

        let snap = workspace_snapshot(usage.total_usage.unwrap(), Some(&sub));
        let primary = &snap.primary;
        assert!((primary.used_percent - 45.7452).abs() < 1e-4);
        assert_eq!(
            primary.reset_description.as_deref(),
            Some("45.75 of 100.00 USD workspace usage")
        );
        assert!(!primary.is_informational);
        assert_eq!(
            snap.login_method.as_deref(),
            Some("payment method on file · access until 2025-01-01")
        );
    }

    #[test]
    fn quota_arithmetic_clamps_and_never_goes_negative() {
        let sub = SubscriptionSummary::parse(COMPLETE_SUB).unwrap();
        let over = workspace_snapshot(25_000.0, Some(&sub));
        assert_eq!(over.primary.used_percent, 100.0);
        assert_eq!(
            over.primary.reset_description.as_deref(),
            Some("250.00 of 100.00 USD workspace usage")
        );

        let negative = workspace_snapshot(-500.0, Some(&sub));
        assert_eq!(negative.primary.used_percent, 0.0);
        assert!(
            negative
                .primary
                .reset_description
                .as_deref()
                .unwrap()
                .starts_with("0.00 of 100.00")
        );
    }

    #[test]
    fn missing_subscription_fields_do_not_become_false_zeroes() {
        let sub = SubscriptionSummary::parse(
            r#"{"object":"billing_subscription","has_payment_method":false,"soft_limit_usd":null,"hard_limit_usd":null,"system_hard_limit_usd":null,"access_until":0}"#,
        )
        .unwrap();
        assert_eq!(sub.effective_cap_usd(), None);

        let snap = workspace_snapshot(1250.0, Some(&sub));
        assert!(snap.primary.is_informational);
        assert_eq!(
            snap.primary.reset_description.as_deref(),
            Some("12.50 USD workspace usage")
        );
        assert_eq!(snap.login_method, None);
    }

    #[test]
    fn missing_total_usage_is_an_error_not_zero() {
        let err = UsageResponse::parse(r#"{"object":"list"}"#).unwrap_err();
        assert!(matches!(err, ProviderError::Parse(_)));
    }

    #[test]
    fn null_total_usage_is_an_error_not_zero() {
        let err = UsageResponse::parse(r#"{"object":"list","total_usage":null}"#).unwrap_err();
        assert!(matches!(err, ProviderError::Parse(_)));
    }

    #[test]
    fn malformed_usage_json_is_a_parse_error() {
        let err = UsageResponse::parse("<html>gateway</html>").unwrap_err();
        assert!(matches!(err, ProviderError::Parse(_)));
    }

    #[test]
    fn malformed_subscription_json_is_a_parse_error() {
        let err = SubscriptionSummary::parse("{not json").unwrap_err();
        assert!(matches!(err, ProviderError::Parse(_)));
    }

    #[test]
    fn http_error_statuses_map_to_expected_errors() {
        assert!(matches!(
            status_error(reqwest::StatusCode::UNAUTHORIZED),
            Some(ProviderError::AuthRequired)
        ));
        assert!(matches!(
            status_error(reqwest::StatusCode::FORBIDDEN),
            Some(ProviderError::AuthRequired)
        ));
        let too_many = status_error(reqwest::StatusCode::TOO_MANY_REQUESTS).unwrap();
        assert!(too_many.to_string().contains("429"));
        assert!(status_error(reqwest::StatusCode::OK).is_none());
    }

    #[tokio::test]
    async fn fetch_fetches_usage_and_subscription_summaries() {
        let mut server = mockito::Server::new_async().await;
        let usage_mock = server
            .mock("GET", "/v1/dashboard/billing/usage")
            .match_header("authorization", "Bearer test-orca-key-123")
            .with_status(200)
            .with_body(COMPLETE_USAGE)
            .create_async()
            .await;
        let sub_mock = server
            .mock("GET", "/v1/dashboard/billing/subscription")
            .match_header("authorization", "Bearer test-orca-key-123")
            .with_status(200)
            .with_body(COMPLETE_SUB)
            .create_async()
            .await;
        let client = reqwest::Client::new();

        let (usage, subscription) = fetch_summaries(
            &client,
            &format!("{}/v1/dashboard/billing/usage", server.url()),
            &format!("{}/v1/dashboard/billing/subscription", server.url()),
            "test-orca-key-123",
        )
        .await
        .unwrap();

        usage_mock.assert_async().await;
        sub_mock.assert_async().await;
        assert!((usage.total_usage.unwrap() - 4574.52).abs() < 1e-9);
        let sub = subscription.unwrap();
        assert_eq!(sub.effective_cap_usd(), Some(100.0));

        let snap = workspace_snapshot(usage.total_usage.unwrap(), Some(&sub));
        assert!((snap.primary.used_percent - 45.7452).abs() < 1e-4);
        assert_eq!(
            snap.login_method.as_deref(),
            Some("payment method on file · access until 2025-01-01")
        );
    }

    #[tokio::test]
    async fn fetch_degrades_when_subscription_call_fails() {
        let mut server = mockito::Server::new_async().await;
        let usage_mock = server
            .mock("GET", "/v1/dashboard/billing/usage")
            .with_status(200)
            .with_body(COMPLETE_USAGE)
            .create_async()
            .await;
        let sub_mock = server
            .mock("GET", "/v1/dashboard/billing/subscription")
            .with_status(500)
            .with_body("oops")
            .create_async()
            .await;
        let client = reqwest::Client::new();

        let (usage, subscription) = fetch_summaries(
            &client,
            &format!("{}/v1/dashboard/billing/usage", server.url()),
            &format!("{}/v1/dashboard/billing/subscription", server.url()),
            "test-orca-key-123",
        )
        .await
        .unwrap();

        usage_mock.assert_async().await;
        sub_mock.assert_async().await;
        assert!(subscription.is_none());
        let snap = workspace_snapshot(usage.total_usage.unwrap(), subscription.as_ref());
        assert!(snap.primary.is_informational);
        assert_eq!(
            snap.primary.reset_description.as_deref(),
            Some("45.75 USD workspace usage")
        );
    }

    #[tokio::test]
    async fn fetch_degrades_to_none_on_malformed_subscription_body() {
        let mut server = mockito::Server::new_async().await;
        let usage_mock = server
            .mock("GET", "/v1/dashboard/billing/usage")
            .with_status(200)
            .with_body(COMPLETE_USAGE)
            .create_async()
            .await;
        let sub_mock = server
            .mock("GET", "/v1/dashboard/billing/subscription")
            .with_status(200)
            .with_body("<html>gateway</html>")
            .create_async()
            .await;
        let client = reqwest::Client::new();

        let (usage, subscription) = fetch_summaries(
            &client,
            &format!("{}/v1/dashboard/billing/usage", server.url()),
            &format!("{}/v1/dashboard/billing/subscription", server.url()),
            "test-orca-key-123",
        )
        .await
        .unwrap();

        usage_mock.assert_async().await;
        sub_mock.assert_async().await;
        assert!(subscription.is_none());
        let snap = workspace_snapshot(usage.total_usage.unwrap(), subscription.as_ref());
        assert!(snap.primary.is_informational);
        assert_eq!(
            snap.primary.reset_description.as_deref(),
            Some("45.75 USD workspace usage")
        );
    }

    #[tokio::test]
    async fn fetch_fails_when_usage_call_is_unauthorized() {
        let mut server = mockito::Server::new_async().await;
        let usage_mock = server
            .mock("GET", "/v1/dashboard/billing/usage")
            .with_status(401)
            .create_async()
            .await;
        let client = reqwest::Client::new();

        let error = fetch_summaries(
            &client,
            &format!("{}/v1/dashboard/billing/usage", server.url()),
            &format!("{}/v1/dashboard/billing/subscription", server.url()),
            "test-orca-key-123",
        )
        .await
        .unwrap_err();

        usage_mock.assert_async().await;
        assert!(matches!(error, ProviderError::AuthRequired));
    }

    #[tokio::test]
    async fn fetch_reports_parse_errors_from_usage_body() {
        let mut server = mockito::Server::new_async().await;
        let usage_mock = server
            .mock("GET", "/v1/dashboard/billing/usage")
            .with_status(200)
            .with_body("<html>gateway</html>")
            .create_async()
            .await;
        let client = reqwest::Client::new();

        let error = fetch_summaries(
            &client,
            &format!("{}/v1/dashboard/billing/usage", server.url()),
            &format!("{}/v1/dashboard/billing/subscription", server.url()),
            "test-orca-key-123",
        )
        .await
        .unwrap_err();

        usage_mock.assert_async().await;
        assert!(matches!(error, ProviderError::Parse(_)));
    }

    #[test]
    fn enormous_sentinel_caps_show_informational_not_zero_percent_quota() {
        let sub = SubscriptionSummary::parse(
            r#"{"has_payment_method":true,"soft_limit_usd":100000000,"hard_limit_usd":100000000,"system_hard_limit_usd":100000000,"access_until":0}"#,
        )
        .unwrap();

        let snap = workspace_snapshot(4574.52, Some(&sub));
        assert!(snap.primary.is_informational);
        assert_eq!(
            snap.primary.reset_description.as_deref(),
            Some("45.75 USD workspace usage")
        );
    }

    #[test]
    fn zero_soft_only_limits_still_compute_quotas() {
        let sub = SubscriptionSummary::parse(
            r#"{"has_payment_method":false,"soft_limit_usd":50,"hard_limit_usd":50,"access_until":0}"#,
        )
        .unwrap();
        let snap = workspace_snapshot(1000.0, Some(&sub));
        assert!((snap.primary.used_percent - 20.0).abs() < 1e-9);
        assert_eq!(
            snap.primary.reset_description.as_deref(),
            Some("10.00 of 50.00 USD workspace usage")
        );
    }

    #[test]
    fn cli_aliases_resolve_to_orcarouter() {
        assert_eq!(
            crate::core::ProviderId::from_cli_name("orcarouter"),
            Some(ProviderId::OrcaRouter)
        );
    }
}
