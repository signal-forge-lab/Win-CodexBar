use crate::core::ProviderError;

use super::{CreditValues, quota_to_usd};

const EDGE_CDP_BALANCE_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$timeoutMs = [Math]::Max(2000, [int]$env:CODEXBAR_AIHUBMIX_CDP_TIMEOUT_MS)
$cts = [System.Threading.CancellationTokenSource]::new()
$cts.CancelAfter($timeoutMs)

function Send-Cdp([System.Net.WebSockets.ClientWebSocket]$ws, [int]$id, [string]$method, $params, [string]$sessionId = '') {
    $msg = @{ id = $id; method = $method; params = $params }
    if ($sessionId) { $msg.sessionId = $sessionId }
    $json = $msg | ConvertTo-Json -Compress -Depth 12
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
                throw 'CDP websocket closed before AIHubMix balance data arrived'
            }
            $stream.Write($buffer, 0, $result.Count)
        } while (-not $result.EndOfMessage)
        $text = [System.Text.Encoding]::UTF8.GetString($stream.ToArray())
        return ($text | ConvertFrom-Json)
    } finally {
        $stream.Dispose()
    }
}

$endpoints = @(Get-CimInstance Win32_Process | Where-Object {
    $_.Name -in @('msedge.exe', 'chrome.exe', 'brave.exe') -and
    $_.CommandLine -match '--remote-debugging-port=([0-9]+)'
} | ForEach-Object {
    if ($_.CommandLine -match '--remote-debugging-port=([0-9]+)') {
        [pscustomobject]@{ port = [int]$matches[1]; wsUrl = $null }
    }
})

$activePortFiles = @(
    (Join-Path $env:LOCALAPPDATA 'Microsoft\Edge\User Data\DevToolsActivePort'),
    (Join-Path $env:LOCALAPPDATA 'Google\Chrome\User Data\DevToolsActivePort')
)
foreach ($activePortFile in $activePortFiles) {
    try {
        if (-not (Test-Path -LiteralPath $activePortFile)) { continue }
        $lines = @(Get-Content -LiteralPath $activePortFile -ErrorAction Stop)
        if ($lines.Count -lt 2) { continue }
        $port = 0
        if (-not [int]::TryParse([string]$lines[0], [ref]$port) -or $port -le 0) { continue }
        $path = [string]$lines[1]
        if ($path -notmatch '^/devtools/browser/[A-Za-z0-9._-]+$') { continue }
        $listener = Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction Stop |
            Where-Object { $_.LocalAddress -in @('127.0.0.1', '::1') } |
            Select-Object -First 1
        if (-not $listener) { continue }
        $owner = Get-Process -Id $listener.OwningProcess -ErrorAction Stop
        if ($owner.ProcessName -notin @('msedge', 'chrome', 'brave')) { continue }
        $endpoints += [pscustomobject]@{
            port = $port
            wsUrl = ("ws://127.0.0.1:{0}{1}" -f $port, $path)
        }
    } catch {
        # Ignore stale or inaccessible browser endpoint metadata.
    }
}

$endpoints = @($endpoints | Group-Object port | ForEach-Object { $_.Group | Select-Object -First 1 })

foreach ($endpoint in $endpoints) {
    $ws = $null
    try {
        $wsUrl = [string]$endpoint.wsUrl
        if (-not $wsUrl) {
            $version = Invoke-RestMethod -Uri ("http://127.0.0.1:{0}/json/version" -f $endpoint.port) -TimeoutSec 2
            $wsUrl = [string]$version.webSocketDebuggerUrl
        }
        if (-not $wsUrl) { continue }

        $ws = [System.Net.WebSockets.ClientWebSocket]::new()
        $ws.ConnectAsync([Uri]$wsUrl, $cts.Token).GetAwaiter().GetResult()

        Send-Cdp $ws 1 'Target.getTargets' @{}
        do { $message = Receive-Cdp $ws } until ($message.id -eq 1)
        if ($message.error) { continue }

        $preferredContexts = @()
        $otherContexts = @()
        foreach ($info in @($message.result.targetInfos)) {
            if ($info.type -ne 'page') { continue }
            $contextId = [string]$info.browserContextId
            $url = [string]$info.url
            if ($url -match '^https://(?:console\.)?aihubmix\.com/' -and $url -notmatch '/login(?:\?|$)') {
                if ($preferredContexts -notcontains $contextId) { $preferredContexts += $contextId }
            } elseif ($otherContexts -notcontains $contextId) {
                $otherContexts += $contextId
            }
        }
        $contexts = @($preferredContexts + $otherContexts | Select-Object -Unique)
        if ($contexts.Count -eq 0) { $contexts = @('') }

        $nextId = 1
        foreach ($contextId in $contexts) {
            $targetId = $null
            $sessionId = $null
            try {
                $createParams = @{ url = 'about:blank'; hidden = $true; background = $true }
                if ($contextId) { $createParams.browserContextId = $contextId }

                $nextId += 1
                $createId = $nextId
                Send-Cdp $ws $createId 'Target.createTarget' $createParams
                do { $message = Receive-Cdp $ws } until ($message.id -eq $createId)
                if ($message.error) { continue }
                $targetId = [string]$message.result.targetId
                if (-not $targetId) { continue }

                $nextId += 1
                $attachId = $nextId
                Send-Cdp $ws $attachId 'Target.attachToTarget' @{ targetId = $targetId; flatten = $true }
                do { $message = Receive-Cdp $ws } until ($message.id -eq $attachId)
                $sessionId = [string]$message.result.sessionId
                if (-not $sessionId) { continue }

                $nextId += 1
                $networkId = $nextId
                Send-Cdp $ws $networkId 'Network.enable' @{} $sessionId
                do { $message = Receive-Cdp $ws } until ($message.id -eq $networkId)

                $nextId += 1
                $navigateId = $nextId
                Send-Cdp $ws $navigateId 'Page.navigate' @{ url = 'https://console.aihubmix.com/topup' } $sessionId

                $requestId = $null
                $authFailed = $false
                while (-not $requestId -and -not $authFailed) {
                    $message = Receive-Cdp $ws
                    if ($message.method -eq 'Network.responseReceived' -and $message.sessionId -eq $sessionId) {
                        $url = [string]$message.params.response.url
                        $status = [int]$message.params.response.status
                        if ($url -match '^https://(?:console\.|api\.)?aihubmix\.com/api/user/self(?:\?|$)') {
                            if ($status -ge 200 -and $status -lt 300) {
                                $requestId = [string]$message.params.requestId
                            } elseif ($status -eq 401 -or $status -eq 403) {
                                $authFailed = $true
                            }
                        }
                    }
                }
                if ($authFailed -or -not $requestId) { continue }

                $nextId += 1
                $bodyId = $nextId
                Send-Cdp $ws $bodyId 'Network.getResponseBody' @{ requestId = $requestId } $sessionId
                do { $message = Receive-Cdp $ws } until ($message.id -eq $bodyId)
                $body = [string]$message.result.body
                if ($message.result.base64Encoded) {
                    $body = [System.Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($body))
                }
                $payload = $body | ConvertFrom-Json
                if ($payload.success -eq $false) { continue }
                $quota = $payload.data.quota
                if ($null -eq $quota) { continue }
                $usedQuota = $payload.data.used_quota
                $quotaText = [Convert]::ToString([double]$quota, [Globalization.CultureInfo]::InvariantCulture)
                $usedText = if ($null -eq $usedQuota) { '' } else { [Convert]::ToString([double]$usedQuota, [Globalization.CultureInfo]::InvariantCulture) }
                [Console]::Out.WriteLine("$quotaText|$usedText")
                exit 0
            } catch {
                # Continue with another browser context without printing session details.
            } finally {
                if ($targetId) {
                    try {
                        $nextId += 1
                        Send-Cdp $ws $nextId 'Target.closeTarget' @{ targetId = $targetId }
                    } catch {}
                }
            }
        }
    } catch {
        # Try the next locally-debuggable Chromium browser. Do not print session details.
    } finally {
        if ($ws) { try { $ws.Dispose() } catch {} }
    }
}

exit 3
"#;

fn parse_credit_values_stdout(stdout: &[u8]) -> Result<CreditValues, ProviderError> {
    let text = String::from_utf8_lossy(stdout);
    let line = text
        .lines()
        .rev()
        .find(|line| line.contains('|'))
        .ok_or(ProviderError::NoCookies)?;
    let (quota_text, used_text) = line.split_once('|').ok_or(ProviderError::NoCookies)?;
    let quota = quota_text
        .trim()
        .parse::<f64>()
        .map_err(|_| ProviderError::NoCookies)?;
    let used_quota = if used_text.trim().is_empty() {
        None
    } else {
        Some(
            used_text
                .trim()
                .parse::<f64>()
                .map_err(|_| ProviderError::NoCookies)?,
        )
    };
    Ok(CreditValues {
        balance_usd: quota_to_usd(quota, "CDP quota")?,
        used_usd: used_quota
            .map(|value| quota_to_usd(value, "CDP used_quota"))
            .transpose()?,
    })
}

#[cfg(windows)]
fn fetch_credit_values_sync(timeout_secs: u64) -> Result<CreditValues, ProviderError> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let timeout_ms = timeout_secs.max(2).saturating_mul(1000).min(30_000);
    let mut command = std::process::Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            EDGE_CDP_BALANCE_SCRIPT,
        ])
        .env("CODEXBAR_AIHUBMIX_CDP_TIMEOUT_MS", timeout_ms.to_string())
        .creation_flags(CREATE_NO_WINDOW);
    let output = command
        .output()
        .map_err(|error| ProviderError::Other(format!("Failed to query Edge CDP: {error}")))?;
    if !output.status.success() {
        return Err(ProviderError::NoCookies);
    }
    parse_credit_values_stdout(&output.stdout)
}

#[cfg(not(windows))]
fn fetch_credit_values_sync(_timeout_secs: u64) -> Result<CreditValues, ProviderError> {
    Err(ProviderError::NoCookies)
}

pub(crate) async fn fetch_credit_values(timeout_secs: u64) -> Result<CreditValues, ProviderError> {
    tokio::task::spawn_blocking(move || fetch_credit_values_sync(timeout_secs))
        .await
        .map_err(|error| ProviderError::Other(format!("AIHubMix CDP task failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_numeric_quota_fields() {
        let values = parse_credit_values_stdout(b"29071257|286403484\r\n").unwrap();
        assert!((values.balance_usd - 58.142514).abs() < 1e-9);
        assert!((values.used_usd.unwrap() - 572.806968).abs() < 1e-9);

        let values = parse_credit_values_stdout(b"500000|\n").unwrap();
        assert_eq!(values.balance_usd, 1.0);
        assert_eq!(values.used_usd, None);
    }

    #[test]
    fn cdp_capture_does_not_extract_auth_material() {
        assert!(!EDGE_CDP_BALANCE_SCRIPT.contains("localStorage"));
        assert!(!EDGE_CDP_BALANCE_SCRIPT.contains("Authorization"));
        assert!(!EDGE_CDP_BALANCE_SCRIPT.contains("Network.getAllCookies"));
        assert!(!EDGE_CDP_BALANCE_SCRIPT.contains("Network.getCookies"));
        assert!(EDGE_CDP_BALANCE_SCRIPT.contains("Network.getResponseBody"));
        assert!(EDGE_CDP_BALANCE_SCRIPT.contains("data.quota"));
        assert!(EDGE_CDP_BALANCE_SCRIPT.contains("data.used_quota"));
        assert!(EDGE_CDP_BALANCE_SCRIPT.contains("DevToolsActivePort"));
        assert!(EDGE_CDP_BALANCE_SCRIPT.contains("OwningProcess"));
    }
}
