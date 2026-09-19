//! b.ai funded-credit usage from an already-open signed-in browser tab.
//!
//! The provider intentionally does not read cookies, localStorage, or the
//! `apiAccessToken` returned by b.ai's user-state endpoint.  Instead it uses a
//! locally exposed Chromium DevTools endpoint only to evaluate same-origin
//! requests inside an existing `chat.b.ai/usage` or `chat.b.ai/purchase` tab.
//! The browser-side result is reduced to non-secret credit totals before it
//! crosses back into CodexBar.

mod cdp;

use async_trait::async_trait;

use crate::core::{
    CostSnapshot, FetchContext, Provider, ProviderError, ProviderFetchResult, ProviderId,
    ProviderMetadata, RateWindow, SourceMode, UsageSnapshot,
};

const DASHBOARD_URL: &str = "https://chat.b.ai/usage";

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CreditValues {
    pub(crate) balance: f64,
    pub(crate) bonus_remaining: f64,
    pub(crate) monthly_spent: f64,
    pub(crate) purchased_total: f64,
    pub(crate) bonus_total: f64,
    pub(crate) funded_total: f64,
}

pub struct BaiProvider {
    metadata: ProviderMetadata,
}

impl BaiProvider {
    pub fn new() -> Self {
        Self {
            metadata: ProviderMetadata {
                id: ProviderId::Bai,
                display_name: "b.ai",
                session_label: "Purchased credits",
                weekly_label: "Funded credits",
                supports_opus: false,
                supports_credits: true,
                default_enabled: false,
                is_primary: false,
                dashboard_url: Some(DASHBOARD_URL),
                status_page_url: None,
            },
        }
    }
}

impl Default for BaiProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn format_points(value: f64) -> String {
    if value >= 1_000_000.0 {
        let millions = value / 1_000_000.0;
        if (millions.fract()).abs() < 1e-9 {
            format!("{millions:.0}M")
        } else {
            format!("{millions:.2}M")
        }
    } else if value >= 1_000.0 {
        let thousands = value / 1_000.0;
        if (thousands.fract()).abs() < 1e-9 {
            format!("{thousands:.0}K")
        } else {
            format!("{thousands:.2}K")
        }
    } else {
        format!("{value:.0}")
    }
}

fn validate(values: CreditValues) -> Result<CreditValues, ProviderError> {
    for (label, value) in [
        ("balance", values.balance),
        ("bonus remaining", values.bonus_remaining),
        ("monthly spent", values.monthly_spent),
        ("purchased total", values.purchased_total),
        ("bonus total", values.bonus_total),
        ("funded total", values.funded_total),
    ] {
        if !value.is_finite() || value < 0.0 {
            return Err(ProviderError::Parse(format!(
                "b.ai {label} was not a finite non-negative number"
            )));
        }
    }
    if values.bonus_remaining > values.balance + 1e-9 {
        return Err(ProviderError::Parse(
            "b.ai bonus balance exceeded total balance".to_string(),
        ));
    }
    let expected_funded = values.purchased_total + values.bonus_total;
    if (values.funded_total - expected_funded).abs() > 0.5 {
        return Err(ProviderError::Parse(
            "b.ai funded total did not match purchase history".to_string(),
        ));
    }
    Ok(values)
}

fn result_from_values(values: CreditValues) -> Result<ProviderFetchResult, ProviderError> {
    let values = validate(values)?;
    let purchased_remaining = (values.balance - values.bonus_remaining).max(0.0);
    let primary =
        if values.purchased_total > 0.0 && values.purchased_total + 1e-9 >= purchased_remaining {
            let purchased_used =
                (values.purchased_total - purchased_remaining).clamp(0.0, values.purchased_total);
            RateWindow::with_details(
                (purchased_used / values.purchased_total) * 100.0,
                None,
                None,
                Some(format!(
                    "{} purchased remaining of {}",
                    format_points(purchased_remaining),
                    format_points(values.purchased_total)
                )),
            )
        } else {
            RateWindow::informational(format!(
                "{} purchased-credit balance",
                format_points(purchased_remaining)
            ))
        };

    let mut usage = UsageSnapshot::new(primary).with_primary_label("Purchased credits");
    if values.funded_total > 0.0 && values.funded_total + 1e-9 >= values.balance {
        let funded_used = (values.funded_total - values.balance).clamp(0.0, values.funded_total);
        usage = usage
            .with_secondary(RateWindow::with_details(
                (funded_used / values.funded_total) * 100.0,
                None,
                None,
                Some(format!(
                    "{} remaining of {} total",
                    format_points(values.balance),
                    format_points(values.funded_total)
                )),
            ))
            .with_secondary_label("Funded credits");
    }
    usage = usage
        .with_extra_rate_window(
            "bonus",
            "Bonus balance",
            RateWindow::informational(format!(
                "{} bonus remaining",
                format_points(values.bonus_remaining)
            )),
        )
        .with_extra_rate_window(
            "funding",
            "Funding",
            RateWindow::informational(format!(
                "{} purchased + {} bonus",
                format_points(values.purchased_total),
                format_points(values.bonus_total)
            )),
        )
        .with_extra_rate_window(
            "monthly",
            "Used this month",
            RateWindow::informational(format!(
                "{} points used this month",
                format_points(values.monthly_spent)
            )),
        );

    let funded_used = if values.funded_total + 1e-9 >= values.balance {
        (values.funded_total - values.balance).max(0.0)
    } else {
        0.0
    };
    let mut cost =
        CostSnapshot::new(funded_used, "points", "Funded credits").with_balance(values.balance);
    if values.funded_total > 0.0 && values.funded_total + 1e-9 >= values.balance {
        cost = cost.with_limit(values.funded_total);
    }

    Ok(ProviderFetchResult::new(usage, "browser-cdp").with_cost(cost))
}

#[async_trait]
impl Provider for BaiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Bai
    }

    fn metadata(&self) -> &ProviderMetadata {
        &self.metadata
    }

    async fn fetch_usage(&self, ctx: &FetchContext) -> Result<ProviderFetchResult, ProviderError> {
        if !matches!(ctx.source_mode, SourceMode::Auto | SourceMode::Web) {
            return Err(ProviderError::UnsupportedSource(ctx.source_mode));
        }
        let values = cdp::fetch_credit_values(ctx.web_timeout).await?;
        result_from_values(values)
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

    fn current_values() -> CreditValues {
        CreditValues {
            balance: 15_000_000.0,
            bonus_remaining: 5_000_000.0,
            monthly_spent: 0.0,
            purchased_total: 10_000_000.0,
            bonus_total: 5_000_000.0,
            funded_total: 15_000_000.0,
        }
    }

    #[test]
    fn purchased_credit_usage_drives_floatbar_percentage() {
        let initial = result_from_values(current_values()).unwrap();
        assert_eq!(initial.usage.primary.used_percent, 0.0);
        assert_eq!(initial.cost.as_ref().unwrap().limit, Some(15_000_000.0));
        assert_eq!(initial.cost.as_ref().unwrap().balance, Some(15_000_000.0));

        let mut later = current_values();
        later.balance = 8_000_000.0;
        later.bonus_remaining = 0.0;
        later.monthly_spent = 7_000_000.0;
        let result = result_from_values(later).unwrap();
        assert!((result.usage.primary.used_percent - 20.0).abs() < 1e-9);
        assert_eq!(result.cost.as_ref().unwrap().used, 7_000_000.0);
        assert!(
            (result.usage.secondary.as_ref().unwrap().used_percent
                - (7_000_000.0 / 15_000_000.0 * 100.0))
                .abs()
                < 1e-9
        );
    }

    #[test]
    fn funding_breakdown_and_monthly_usage_stay_informational() {
        let result = result_from_values(current_values()).unwrap();
        let funding = result
            .usage
            .extra_rate_windows
            .iter()
            .find(|window| window.id == "funding")
            .unwrap();
        assert!(funding.window.is_informational);
        assert_eq!(
            funding.window.reset_description.as_deref(),
            Some("10M purchased + 5M bonus")
        );
        let bonus = result
            .usage
            .extra_rate_windows
            .iter()
            .find(|window| window.id == "bonus")
            .unwrap();
        assert!(bonus.window.is_informational);
        assert!(!result.usage.secondary.as_ref().unwrap().is_informational);
    }

    #[test]
    fn bonus_consumption_does_not_count_as_purchased_credit_usage() {
        let mut values = current_values();
        values.balance = 12_000_000.0;
        values.bonus_remaining = 2_000_000.0;
        values.monthly_spent = 3_000_000.0;
        let result = result_from_values(values).unwrap();
        assert_eq!(result.usage.primary.used_percent, 0.0);
        assert!((result.usage.secondary.as_ref().unwrap().used_percent - 20.0).abs() < 1e-9);
    }

    #[test]
    fn invalid_or_incomplete_funding_fails_closed_to_balance_only() {
        let mut values = current_values();
        values.funded_total = 14_000_000.0;
        assert!(result_from_values(values).is_err());

        let mut no_history = current_values();
        no_history.purchased_total = 0.0;
        no_history.bonus_total = 0.0;
        no_history.funded_total = 0.0;
        let result = result_from_values(no_history).unwrap();
        assert!(result.usage.primary.is_informational);
        assert_eq!(result.cost.as_ref().unwrap().limit, None);
    }

    #[test]
    fn metadata_is_browser_session_specific() {
        let provider = BaiProvider::new();
        assert_eq!(provider.id(), ProviderId::Bai);
        assert_eq!(provider.metadata().display_name, "b.ai");
        assert_eq!(provider.metadata().dashboard_url, Some(DASHBOARD_URL));
        assert!(provider.metadata().supports_credits);
        assert_eq!(
            provider.available_sources(),
            vec![SourceMode::Auto, SourceMode::Web]
        );
    }

    #[test]
    fn compact_point_labels_match_bai_scale() {
        assert_eq!(format_points(15_000_000.0), "15M");
        assert_eq!(format_points(1_250_000.0), "1.25M");
        assert_eq!(format_points(25_000.0), "25K");
    }
}
