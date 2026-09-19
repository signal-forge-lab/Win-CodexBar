use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;

use crate::core::ProviderError;

use super::CreditValues;

const ORIGIN: &str = "https://chat.b.ai";
const REFERER: &str = "https://chat.b.ai/usage";
const MAX_ORDER_PAGES: u64 = 100;
const ORDER_PAGE_SIZE: u64 = 100;

#[derive(Debug, Deserialize)]
struct TrpcEnvelope<T> {
    result: TrpcResult<T>,
}

#[derive(Debug, Deserialize)]
struct TrpcResult<T> {
    data: TrpcData<T>,
}

#[derive(Debug, Deserialize)]
struct TrpcData<T> {
    json: T,
}

#[derive(Debug, Deserialize)]
struct PointsPayload {
    points_balance: f64,
    points_expiring: f64,
}

#[derive(Debug, Deserialize)]
struct SummaryPayload {
    monthly_spent: f64,
}

#[derive(Debug, Deserialize)]
struct OrdersPayload {
    data: Vec<OrderRecord>,
    total: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OrderRecord {
    status: String,
    #[serde(rename = "type")]
    #[serde(default)]
    order_type: String,
    #[serde(default)]
    recharge_type: String,
    #[serde(default)]
    recipient_relation: Option<String>,
    points: f64,
}

fn trpc_url(base_url: &str, procedure: &str, input: &str) -> Result<Url, ProviderError> {
    let mut url = Url::parse(&format!("{base_url}/trpc/lambda/{procedure}"))
        .map_err(|error| ProviderError::Other(format!("Invalid b.ai URL: {error}")))?;
    url.query_pairs_mut()
        .append_pair("batch", "1")
        .append_pair("input", input);
    Ok(url)
}

fn empty_input() -> String {
    r#"{"0":{"json":null,"meta":{"values":["undefined"],"v":1}}}"#.to_string()
}

fn orders_input(page: u64) -> String {
    serde_json::json!({
        "0": {
            "json": {
                "page": page,
                "pageSize": ORDER_PAGE_SIZE,
                "sortBy": "createdAt",
                "order": "desc"
            }
        }
    })
    .to_string()
}

async fn fetch_trpc<T: for<'de> Deserialize<'de>>(
    client: &Client,
    cookie_header: &str,
    base_url: &str,
    procedure: &str,
    input: &str,
) -> Result<T, ProviderError> {
    let response = client
        .get(trpc_url(base_url, procedure, input)?)
        .header(reqwest::header::COOKIE, cookie_header)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::REFERER, REFERER)
        .send()
        .await?;

    let status = response.status();
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Err(ProviderError::AuthRequired);
    }
    if !status.is_success() {
        return Err(ProviderError::Other(format!(
            "b.ai {procedure} returned HTTP {status}"
        )));
    }

    let body = response.text().await?;
    let mut envelopes: Vec<TrpcEnvelope<T>> = serde_json::from_str(&body).map_err(|error| {
        ProviderError::Parse(format!("Failed to parse b.ai {procedure}: {error}"))
    })?;
    if envelopes.len() != 1 {
        return Err(ProviderError::Parse(format!(
            "b.ai {procedure} returned an unexpected batch size"
        )));
    }
    Ok(envelopes.remove(0).result.data.json)
}

pub(crate) async fn fetch_credit_values(
    client: &Client,
    cookie_header: &str,
    base_url: &str,
) -> Result<CreditValues, ProviderError> {
    let empty = empty_input();
    let points: PointsPayload =
        fetch_trpc(client, cookie_header, base_url, "usage.points", &empty).await?;
    let summary: SummaryPayload =
        fetch_trpc(client, cookie_header, base_url, "usage.summary", &empty).await?;

    let mut purchased_total = 0.0;
    let mut bonus_total = 0.0;
    let mut page = 1;
    loop {
        let orders: OrdersPayload = fetch_trpc(
            client,
            cookie_header,
            base_url,
            "order.listOrders",
            &orders_input(page),
        )
        .await?;

        for order in &orders.data {
            if order.status != "success"
                || !order.points.is_finite()
                || order.points <= 0.0
                || order
                    .recipient_relation
                    .as_deref()
                    .is_some_and(|relation| relation != "self")
            {
                continue;
            }
            if order.order_type == "purchase" || order.recharge_type == "fiat" {
                purchased_total += order.points;
            } else if order.order_type == "bonus" || order.recharge_type == "bonus" {
                bonus_total += order.points;
            }
        }

        if page.saturating_mul(ORDER_PAGE_SIZE) >= orders.total {
            break;
        }
        if page >= MAX_ORDER_PAGES {
            return Err(ProviderError::Parse(
                "b.ai order history exceeds the bounded reader".to_string(),
            ));
        }
        page += 1;
    }

    Ok(CreditValues {
        balance: points.points_balance,
        bonus_remaining: points.points_expiring,
        monthly_spent: summary.monthly_spent,
        purchased_total,
        bonus_total,
        funded_total: purchased_total + bonus_total,
    })
}

pub(crate) async fn fetch_from_browser_session(
    cookie_header: &str,
    timeout_secs: u64,
) -> Result<CreditValues, ProviderError> {
    let client = crate::core::credentialed_http_client_builder()
        .timeout(std::time::Duration::from_secs(timeout_secs.max(1)))
        .build()
        .map_err(|error| ProviderError::Other(format!("Failed to build b.ai client: {error}")))?;
    fetch_credit_values(&client, cookie_header, ORIGIN).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trpc<T: serde::Serialize>(value: T) -> String {
        serde_json::json!([{"result":{"data":{"json":value}}}]).to_string()
    }

    #[tokio::test]
    async fn browser_cookie_session_fetches_usage_and_funding_history() {
        let mut server = mockito::Server::new_async().await;
        let cookie = "session=test-session";
        let points = server
            .mock("GET", "/trpc/lambda/usage.points")
            .match_header("cookie", cookie)
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(trpc(serde_json::json!({
                "points_balance": 15_000_000,
                "points_expiring": 5_000_000
            })))
            .create_async()
            .await;
        let summary = server
            .mock("GET", "/trpc/lambda/usage.summary")
            .match_header("cookie", cookie)
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(trpc(serde_json::json!({
                "monthly_chart": [],
                "monthly_spent": 0,
                "points_balance": 15_000_000
            })))
            .create_async()
            .await;
        let orders = server
            .mock("GET", "/trpc/lambda/order.listOrders")
            .match_header("cookie", cookie)
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(trpc(serde_json::json!({
                "data": [
                    {
                        "status": "success",
                        "type": "bonus",
                        "rechargeType": "bonus",
                        "points": 5_000_000
                    },
                    {
                        "status": "success",
                        "type": "purchase",
                        "rechargeType": "fiat",
                        "recipientRelation": "self",
                        "points": 10_000_000
                    }
                ],
                "page": 1,
                "pageSize": 100,
                "total": 2
            })))
            .create_async()
            .await;

        let values = fetch_credit_values(&Client::new(), cookie, &server.url())
            .await
            .unwrap();
        assert_eq!(
            values,
            CreditValues {
                balance: 15_000_000.0,
                bonus_remaining: 5_000_000.0,
                monthly_spent: 0.0,
                purchased_total: 10_000_000.0,
                bonus_total: 5_000_000.0,
                funded_total: 15_000_000.0,
            }
        );
        points.assert_async().await;
        summary.assert_async().await;
        orders.assert_async().await;
    }

    #[tokio::test]
    async fn auth_failures_are_reported_without_parsing_body() {
        let mut server = mockito::Server::new_async().await;
        let unauthorized = server
            .mock("GET", "/trpc/lambda/usage.points")
            .match_query(mockito::Matcher::Any)
            .with_status(401)
            .with_body("not-json")
            .create_async()
            .await;

        let error = fetch_credit_values(&Client::new(), "expired=1", &server.url())
            .await
            .unwrap_err();
        assert!(matches!(error, ProviderError::AuthRequired));
        unauthorized.assert_async().await;
    }

    #[tokio::test]
    async fn gifted_purchases_do_not_inflate_the_local_funding_anchor() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/trpc/lambda/usage.points")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(trpc(serde_json::json!({
                "points_balance": 10_000_000,
                "points_expiring": 0
            })))
            .create_async()
            .await;
        server
            .mock("GET", "/trpc/lambda/usage.summary")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(trpc(serde_json::json!({"monthly_spent": 0})))
            .create_async()
            .await;
        server
            .mock("GET", "/trpc/lambda/order.listOrders")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(trpc(serde_json::json!({
                "data": [
                    {
                        "status": "success",
                        "type": "purchase",
                        "rechargeType": "fiat",
                        "recipientRelation": "self",
                        "points": 10_000_000
                    },
                    {
                        "status": "success",
                        "type": "purchase",
                        "rechargeType": "fiat",
                        "recipientRelation": "gift",
                        "points": 20_000_000
                    }
                ],
                "total": 2
            })))
            .create_async()
            .await;

        let values = fetch_credit_values(&Client::new(), "session=test", &server.url())
            .await
            .unwrap();
        assert_eq!(values.purchased_total, 10_000_000.0);
    }

    #[tokio::test]
    async fn funding_history_accepts_either_bai_classification_field() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("GET", "/trpc/lambda/usage.points")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(trpc(serde_json::json!({
                "points_balance": 15_000_000,
                "points_expiring": 5_000_000
            })))
            .create_async()
            .await;
        server
            .mock("GET", "/trpc/lambda/usage.summary")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(trpc(serde_json::json!({"monthly_spent": 0})))
            .create_async()
            .await;
        server
            .mock("GET", "/trpc/lambda/order.listOrders")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(trpc(serde_json::json!({
                "data": [
                    {
                        "status": "success",
                        "type": "purchase",
                        "recipientRelation": "self",
                        "points": 10_000_000
                    },
                    {
                        "status": "success",
                        "rechargeType": "bonus",
                        "points": 5_000_000
                    }
                ],
                "total": 2
            })))
            .create_async()
            .await;

        let values = fetch_credit_values(&Client::new(), "session=test", &server.url())
            .await
            .unwrap();
        assert_eq!(values.purchased_total, 10_000_000.0);
        assert_eq!(values.bonus_total, 5_000_000.0);
        assert_eq!(values.funded_total, 15_000_000.0);
    }
}
