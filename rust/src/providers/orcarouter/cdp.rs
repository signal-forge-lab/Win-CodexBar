use crate::core::ProviderError;

const EDGE_CDP_WALLET_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$timeoutMs = [Math]::Max(2000, [int]$env:CODEXBAR_ORCA_CDP_TIMEOUT_MS)
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
                throw 'CDP websocket closed before wallet data arrived'
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
} | Sort-Object -Unique)

foreach ($port in $ports) {
    $ws = $null
    try {
        $version = Invoke-RestMethod -Uri ("http://127.0.0.1:{0}/json/version" -f $port) -TimeoutSec 2
        if (-not $version.webSocketDebuggerUrl) { continue }

        $ws = [System.Net.WebSockets.ClientWebSocket]::new()
        $ws.ConnectAsync([Uri]$version.webSocketDebuggerUrl, $cts.Token).GetAwaiter().GetResult()

        Send-Cdp $ws 1 'Target.getTargets' @{}
        do { $message = Receive-Cdp $ws } until ($message.id -eq 1)
        if ($message.error) { continue }

        $preferredContexts = @()
        $otherContexts = @()
        foreach ($info in @($message.result.targetInfos)) {
            if ($info.type -ne 'page') { continue }
            $contextId = [string]$info.browserContextId
            $url = [string]$info.url
            if ($url -match '^https://(?:www\.)?orcarouter\.ai/' -and $url -notmatch '/login(?:\?|$)') {
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
                Send-Cdp $ws $navigateId 'Page.navigate' @{ url = 'https://www.orcarouter.ai/console/billing' } $sessionId

                $requestId = $null
                $authFailed = $false
                while (-not $requestId -and -not $authFailed) {
                    $message = Receive-Cdp $ws
                    if ($message.method -eq 'Network.responseReceived' -and $message.sessionId -eq $sessionId) {
                        $url = [string]$message.params.response.url
                        $status = [int]$message.params.response.status
                        if ($url -match '/api/user/self(?:\?|$)') {
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
                $quota = $payload.data.active_workspace.wallet_quota
                if ($null -eq $quota) { continue }
                [Console]::Out.WriteLine([Convert]::ToString([double]$quota, [Globalization.CultureInfo]::InvariantCulture))
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

fn parse_wallet_quota_stdout(stdout: &[u8]) -> Result<f64, ProviderError> {
    let text = String::from_utf8_lossy(stdout);
    let quota = text
        .lines()
        .rev()
        .find_map(|line| line.trim().parse::<f64>().ok())
        .ok_or_else(|| ProviderError::NoCookies)?;
    if !quota.is_finite() || quota < 0.0 {
        return Err(ProviderError::Parse(
            "OrcaRouter CDP wallet quota was not a finite non-negative number".to_string(),
        ));
    }
    Ok(quota)
}

#[cfg(windows)]
fn fetch_wallet_quota_sync(timeout_secs: u64) -> Result<f64, ProviderError> {
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
            EDGE_CDP_WALLET_SCRIPT,
        ])
        .env("CODEXBAR_ORCA_CDP_TIMEOUT_MS", timeout_ms.to_string())
        .creation_flags(CREATE_NO_WINDOW);
    let output = command
        .output()
        .map_err(|error| ProviderError::Other(format!("Failed to query Edge CDP: {error}")))?;
    if !output.status.success() {
        return Err(ProviderError::NoCookies);
    }
    parse_wallet_quota_stdout(&output.stdout)
}

#[cfg(not(windows))]
fn fetch_wallet_quota_sync(_timeout_secs: u64) -> Result<f64, ProviderError> {
    Err(ProviderError::NoCookies)
}

pub(crate) async fn fetch_wallet_quota(timeout_secs: u64) -> Result<f64, ProviderError> {
    tokio::task::spawn_blocking(move || fetch_wallet_quota_sync(timeout_secs))
        .await
        .map_err(|error| ProviderError::Other(format!("OrcaRouter CDP task failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numeric_wallet_quota_without_exposing_browser_state() {
        assert_eq!(
            parse_wallet_quota_stdout(b"2505000\r\n").unwrap(),
            2_505_000.0
        );
        assert!(parse_wallet_quota_stdout(b"not-a-number\n").is_err());
    }

    #[test]
    fn cdp_capture_does_not_extract_auth_material() {
        assert!(!EDGE_CDP_WALLET_SCRIPT.contains("localStorage"));
        assert!(!EDGE_CDP_WALLET_SCRIPT.contains("Authorization"));
        assert!(!EDGE_CDP_WALLET_SCRIPT.contains("New-Api-User"));
        assert!(EDGE_CDP_WALLET_SCRIPT.contains("Network.getResponseBody"));
        assert!(EDGE_CDP_WALLET_SCRIPT.contains("wallet_quota"));
    }
}
