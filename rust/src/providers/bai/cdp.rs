use serde::Deserialize;

use crate::core::ProviderError;

use super::CreditValues;

const EDGE_CDP_CREDITS_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$timeoutMs = [Math]::Max(2000, [int]$env:CODEXBAR_BAI_CDP_TIMEOUT_MS)
$cts = [System.Threading.CancellationTokenSource]::new()
$cts.CancelAfter($timeoutMs)

function Send-Cdp([System.Net.WebSockets.ClientWebSocket]$ws, [int]$id, [string]$method, $params, [string]$sessionId = '') {
    $msg = @{ id = $id; method = $method; params = $params }
    if ($sessionId) { $msg.sessionId = $sessionId }
    $json = $msg | ConvertTo-Json -Compress -Depth 16
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
    $seg = [System.ArraySegment[byte]]::new($bytes)
    $ws.SendAsync($seg, [System.Net.WebSockets.WebSocketMessageType]::Text, $true, $cts.Token).GetAwaiter().GetResult()
}

function Receive-Cdp([System.Net.WebSockets.ClientWebSocket]$ws) {
    $stream = [System.IO.MemoryStream]::new()
    try {
        do {
            $buffer = New-Object byte[] 65536
            $seg = [System.ArraySegment[byte]]::new($buffer)
            $result = $ws.ReceiveAsync($seg, $cts.Token).GetAwaiter().GetResult()
            if ($result.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) {
                throw 'CDP websocket closed before b.ai usage data arrived'
            }
            $stream.Write($buffer, 0, $result.Count)
        } while (-not $result.EndOfMessage)
        $text = [System.Text.Encoding]::UTF8.GetString($stream.ToArray())
        return ($text | ConvertFrom-Json)
    } finally {
        $stream.Dispose()
    }
}

$ports = @(Get-CimInstance Win32_Process | Where-Object {
    $_.Name -in @('msedge.exe', 'chrome.exe', 'brave.exe') -and
    $_.CommandLine -match '--remote-debugging-port=([0-9]+)'
} | ForEach-Object {
    if ($_.CommandLine -match '--remote-debugging-port=([0-9]+)') { [int]$matches[1] }
} | Select-Object -Unique)

foreach ($port in $ports) {
    try {
        $targets = Invoke-RestMethod -Uri ("http://127.0.0.1:{0}/json" -f $port) -TimeoutSec 2
    } catch {
        continue
    }
    foreach ($target in $targets) {
        $url = [string]$target.url
        if ($target.type -ne 'page' -or
            $url -notmatch '^https://chat\.b\.ai/(?:usage|purchase)(?:[/?#]|$)' -or
            -not $target.webSocketDebuggerUrl) {
            continue
        }

        $ws = $null
        try {
            $ws = [System.Net.WebSockets.ClientWebSocket]::new()
            $ws.ConnectAsync([Uri][string]$target.webSocketDebuggerUrl, $cts.Token).GetAwaiter().GetResult()

            # Edge may freeze hidden tabs. Reactivating the lifecycle keeps the
            # tab hidden but allows same-origin fetch promises to make progress.
            Send-Cdp $ws 90 'Page.setWebLifecycleState' @{ state = 'active' }
            do { $message = Receive-Cdp $ws } until ($message.id -eq 90)
            if ($message.error) { continue }

            $script = @'
(async () => {
  const requestJson = async (url) => {
    const response = await fetch(url, {
      credentials: 'include',
      cache: 'no-store',
      headers: { Accept: 'application/json' }
    });
    if (!response.ok) throw new Error(`b.ai request failed: ${response.status}`);
    return response.json();
  };
  const trpcJson = (body) => body?.[0]?.result?.data?.json ?? null;
  const emptyInput = encodeURIComponent(JSON.stringify({
    0: { json: null, meta: { values: ['undefined'], v: 1 } }
  }));
  const points = trpcJson(await requestJson(`/trpc/lambda/usage.points?batch=1&input=${emptyInput}`));
  const summary = trpcJson(await requestJson(`/trpc/lambda/usage.summary?batch=1&input=${emptyInput}`));
  if (!points || !summary) throw new Error('b.ai usage response was incomplete');

  let purchased = 0;
  let bonus = 0;
  let page = 1;
  const pageSize = 100;
  while (true) {
    const input = encodeURIComponent(JSON.stringify({
      0: { json: { page, pageSize, sortBy: 'createdAt', order: 'desc' } }
    }));
    const orders = trpcJson(await requestJson(`/trpc/lambda/order.listOrders?batch=1&input=${input}`));
    if (!orders || !Array.isArray(orders.data)) throw new Error('b.ai order history was incomplete');
    for (const order of orders.data) {
      if (order?.status !== 'success') continue;
      if (order?.recipientRelation && order.recipientRelation !== 'self') continue;
      const value = Number(order?.points);
      if (!Number.isFinite(value) || value <= 0) continue;
      if (order?.type === 'purchase' || order?.rechargeType === 'fiat') purchased += value;
      else if (order?.type === 'bonus' || order?.rechargeType === 'bonus') bonus += value;
    }
    const total = Number(orders.total);
    if (!Number.isFinite(total) || total < 0) throw new Error('b.ai order total was invalid');
    if (page * pageSize >= total) break;
    if (page >= 100) throw new Error('b.ai order history exceeds the bounded reader');
    page += 1;
  }

  const payload = {
    balance: Number(points.points_balance),
    bonus_remaining: Number(points.points_expiring),
    monthly_spent: Number(summary.monthly_spent),
    purchased_total: purchased,
    bonus_total: bonus,
    funded_total: purchased + bonus
  };
  if (Object.values(payload).some((value) => !Number.isFinite(value) || value < 0)) {
    throw new Error('b.ai sanitized credit totals were invalid');
  }
  return JSON.stringify(payload);
})()
'@
            Send-Cdp $ws 1 'Runtime.evaluate' @{
                expression = $script
                awaitPromise = $true
                returnByValue = $true
            }
            do { $message = Receive-Cdp $ws } until ($message.id -eq 1)
            if ($message.error -or $message.result.exceptionDetails) { continue }
            $value = [string]$message.result.result.value
            if (-not $value) { continue }
            [Console]::Out.WriteLine($value)
            exit 0
        } catch {
            # Try another already-open b.ai tab/browser without exposing session state.
        } finally {
            if ($ws) { try { $ws.Dispose() } catch {} }
        }
    }
}

exit 3
"#;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserCreditValues {
    balance: f64,
    bonus_remaining: f64,
    monthly_spent: f64,
    purchased_total: f64,
    bonus_total: f64,
    funded_total: f64,
}

fn parse_credit_values_stdout(stdout: &[u8]) -> Result<CreditValues, ProviderError> {
    let text = String::from_utf8_lossy(stdout);
    let line = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .ok_or(ProviderError::NoCookies)?;
    let parsed: BrowserCreditValues = serde_json::from_str(line.trim()).map_err(|error| {
        ProviderError::Parse(format!("Invalid b.ai browser usage response: {error}"))
    })?;
    Ok(CreditValues {
        balance: parsed.balance,
        bonus_remaining: parsed.bonus_remaining,
        monthly_spent: parsed.monthly_spent,
        purchased_total: parsed.purchased_total,
        bonus_total: parsed.bonus_total,
        funded_total: parsed.funded_total,
    })
}

#[cfg(windows)]
fn fetch_credit_values_sync(timeout_secs: u64) -> Result<CreditValues, ProviderError> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let timeout_ms = timeout_secs.max(2).saturating_mul(1000).min(30_000);
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            EDGE_CDP_CREDITS_SCRIPT,
        ])
        .env("CODEXBAR_BAI_CDP_TIMEOUT_MS", timeout_ms.to_string())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| {
            ProviderError::Other(format!("Failed to query b.ai browser tab: {error}"))
        })?;
    if !output.status.success() {
        return Err(ProviderError::NotInstalled(
            "No signed-in b.ai Usage/Top up tab was available through a local Chromium DevTools session. Keep https://chat.b.ai/usage open in the Converlay profile (or another Edge/Chrome profile with remote debugging enabled), then refresh CodexBar."
                .to_string(),
        ));
    }
    parse_credit_values_stdout(&output.stdout)
}

#[cfg(not(windows))]
fn fetch_credit_values_sync(_timeout_secs: u64) -> Result<CreditValues, ProviderError> {
    Err(ProviderError::NotInstalled(
        "b.ai browser-session usage is currently available on Windows only.".to_string(),
    ))
}

pub(crate) async fn fetch_credit_values(timeout_secs: u64) -> Result<CreditValues, ProviderError> {
    tokio::task::spawn_blocking(move || fetch_credit_values_sync(timeout_secs))
        .await
        .map_err(|error| ProviderError::Other(format!("b.ai browser task failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_sanitized_credit_totals() {
        let values = parse_credit_values_stdout(
            br#"{"balance":15000000,"bonus_remaining":5000000,"monthly_spent":0,"purchased_total":10000000,"bonus_total":5000000,"funded_total":15000000}"#,
        )
        .unwrap();
        assert_eq!(values.balance, 15_000_000.0);
        assert_eq!(values.purchased_total, 10_000_000.0);
        assert_eq!(values.bonus_total, 5_000_000.0);
    }

    #[test]
    fn rejects_secret_bearing_browser_output() {
        assert!(
            parse_credit_values_stdout(
                br#"{"balance":15000000,"bonus_remaining":5000000,"monthly_spent":0,"purchased_total":10000000,"bonus_total":5000000,"funded_total":15000000,"apiAccessToken":"must-not-cross-boundary"}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn cdp_reader_is_passive_and_does_not_extract_auth_material() {
        assert!(!EDGE_CDP_CREDITS_SCRIPT.contains("Target.createTarget"));
        assert!(!EDGE_CDP_CREDITS_SCRIPT.contains("Page.navigate"));
        assert!(EDGE_CDP_CREDITS_SCRIPT.contains("Page.setWebLifecycleState"));
        assert!(!EDGE_CDP_CREDITS_SCRIPT.contains("localStorage"));
        assert!(!EDGE_CDP_CREDITS_SCRIPT.contains("document.cookie"));
        assert!(!EDGE_CDP_CREDITS_SCRIPT.contains("apiAccessToken"));
        assert!(!EDGE_CDP_CREDITS_SCRIPT.contains("Authorization"));
        assert!(EDGE_CDP_CREDITS_SCRIPT.contains("usage.points"));
        assert!(EDGE_CDP_CREDITS_SCRIPT.contains("usage.summary"));
        assert!(EDGE_CDP_CREDITS_SCRIPT.contains("order.listOrders"));
    }
}
