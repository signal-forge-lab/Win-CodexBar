use crate::core::ProviderError;

use super::SpendPayload;

// Keep the embedded PowerShell/JavaScript ASCII-only. Windows PowerShell can
// otherwise reinterpret source literals when this script crosses process
// boundaries. Localized AI Studio labels and currency glyphs are represented
// with JavaScript Unicode escapes instead.
const AI_STUDIO_CDP_SPEND_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$timeoutMs = [Math]::Max(2500, [Math]::Min(30000, [int]$env:CODEXBAR_GEMINI_API_CDP_TIMEOUT_MS))

function Send-Cdp([System.Net.WebSockets.ClientWebSocket]$ws, [System.Threading.CancellationTokenSource]$cts, [int]$id, [string]$method, $params, [string]$sessionId = '') {
    $msg = @{ id = $id; method = $method; params = $params }
    if ($sessionId) { $msg.sessionId = $sessionId }
    $json = $msg | ConvertTo-Json -Compress -Depth 12
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
    $seg = [System.ArraySegment[byte]]::new($bytes)
    [void]$ws.SendAsync($seg, [System.Net.WebSockets.WebSocketMessageType]::Text, $true, $cts.Token).GetAwaiter().GetResult()
}

function Receive-Cdp([System.Net.WebSockets.ClientWebSocket]$ws, [System.Threading.CancellationTokenSource]$cts) {
    $stream = [System.IO.MemoryStream]::new()
    try {
        do {
            $buffer = New-Object byte[] 65536
            $seg = [System.ArraySegment[byte]]::new($buffer)
            $result = $ws.ReceiveAsync($seg, $cts.Token).GetAwaiter().GetResult()
            if ($result.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) {
                throw 'CDP websocket closed before Gemini API spend data arrived'
            }
            $stream.Write($buffer, 0, $result.Count)
        } while (-not $result.EndOfMessage)
        $text = [System.Text.Encoding]::UTF8.GetString($stream.ToArray())
        return ($text | ConvertFrom-Json)
    } finally {
        $stream.Dispose()
    }
}

$extractExpression = @'
(() => {
  if (location.origin !== 'https://aistudio.google.com' || location.pathname !== '/spend') return null;

  const currencyCode = (symbol) => {
    if (symbol === '$') return 'USD';
    if (symbol === '\u20ac') return 'EUR';
    if (symbol === '\u00a3') return 'GBP';
    if (symbol === '\u00a5' || symbol === '\uffe5') return 'JPY';
    return null;
  };

  const parseMoney = (raw) => {
    const text = String(raw || '').replace(/\u00a0/g, ' ').trim();
    const match = text.match(/^([$\u20ac\u00a3\u00a5\uffe5])?\s*([0-9][0-9,]*(?:\.[0-9]+)?)\s*(USD|EUR|GBP|JPY)?$/i);
    if (!match) return null;
    const amount = Number(match[2].replace(/,/g, ''));
    if (!Number.isFinite(amount) || amount < 0) return null;
    const explicit = match[3] ? match[3].toUpperCase() : null;
    const inferred = match[1] ? currencyCode(match[1]) : null;
    if (explicit && inferred && explicit !== inferred) return null;
    const currency = explicit || inferred;
    return currency ? { amount, currency } : null;
  };

  const lines = (document.body?.innerText || '')
    .replace(/\u00a0/g, ' ')
    .split(/\n+/)
    .map((line) => line.trim())
    .filter(Boolean);

  const capture = (pattern, lookahead) => {
    for (let index = 0; index < lines.length; index += 1) {
      const match = lines[index].match(pattern);
      if (!match) continue;
      const inline = parseMoney(match[1] || '');
      if (inline) return inline;
      for (let offset = 1; offset <= lookahead; offset += 1) {
        const money = parseMoney(lines[index + offset] || '');
        if (money) return money;
      }
    }
    return null;
  };

  const spend = capture(
    /^\s*(?:\u7dcf\u8cbb\u7528|total cost|current(?: billing)? period spend|current spend|spend this month|this month(?:'s)? spend|total spend)\s*[:\uFF1A-]?\s*(.*)$/i,
    1
  );
  if (!spend) return null;

  const capIndex = lines.findIndex((line) =>
    (line.includes('\u8cbb\u7528') && line.includes('\u4e0a\u9650')) ||
    /monthly spend cap|spending limit|spend limit|monthly limit|budget/i.test(line)
  );
  if (capIndex < 0) return null;
  const capPanel = lines.slice(capIndex, capIndex + 8);
  let capUsed = null;
  let limit = null;
  let capStateKnown = false;

  const inlinePair = capPanel.join(' ').match(/([$\u20ac\u00a3\u00a5\uffe5]\s*[0-9][0-9,]*(?:\.[0-9]+)?\s*(?:USD|EUR|GBP|JPY)?)\s*\/\s*([$\u20ac\u00a3\u00a5\uffe5]\s*[0-9][0-9,]*(?:\.[0-9]+)?\s*(?:USD|EUR|GBP|JPY)?)/i);
  if (inlinePair) {
    capUsed = parseMoney(inlinePair[1]);
    limit = parseMoney(inlinePair[2]);
    capStateKnown = Boolean(capUsed && limit);
  } else {
    const slashIndex = capPanel.findIndex((line) => line === '/' || line.includes('/'));
    if (slashIndex >= 0) {
      for (let index = slashIndex - 1; index >= 0 && !capUsed; index -= 1) {
        capUsed = parseMoney(capPanel[index]);
      }
      for (let index = slashIndex + 1; index < capPanel.length && !limit; index += 1) {
        limit = parseMoney(capPanel[index]);
      }
      if (capUsed && limit) {
        capStateKnown = true;
      } else if (capUsed) {
        const afterSlash = capPanel.slice(slashIndex + 1, slashIndex + 4).join(' ');
        capStateKnown = /^(?:\s*[-\u2013\u2014]\s*|.*(?:not set|no limit|unlimited).*)$/i.test(afterSlash);
      }
    } else {
      const nearby = capPanel.slice(1, 5).join(' ');
      const japaneseUnset = nearby.includes('\u8cbb\u7528\u306e\u4e0a\u9650\u3092\u8a2d\u5b9a')
        && !nearby.includes('\u7de8\u96c6');
      const englishUnset = /set\s+(?:a\s+)?(?:monthly\s+)?(?:spend\s+)?(?:cap|limit)/i.test(nearby)
        && !/edit/i.test(nearby);
      capStateKnown = japaneseUnset || englishUnset;
    }
  }
  if (!capStateKnown) return null;
  if (capUsed && capUsed.currency !== spend.currency) return null;
  if (limit && limit.currency !== spend.currency) return null;

  const safeText = (value) => {
    const text = String(value || '').trim();
    if (!text || text.length > 64 || text.includes('@')) return null;
    if (/\b(?:billing\s+account|account\s+id)\b/i.test(text)) return null;
    return /^[\p{L}\p{N} _.,/\u2013\u2014-]{1,64}$/u.test(text) ? text : null;
  };

  let period = null;
  for (const line of lines) {
    const match = line.match(/^\s*(?:billing period|period)\s*[:\uFF1A-]\s*(.{1,64})$/i);
    if (match) {
      period = safeText(match[1]);
      break;
    }
  }
  if (!period) {
    const range = lines.find((line) => /^[A-Z][a-z]+\s+\d{1,2}\s*[-\u2013\u2014]\s*[A-Z][a-z]+\s+\d{1,2},\s+\d{4}$/.test(line));
    period = safeText(range) || 'Current period';
  }

  let project = null;
  const inlineProject = lines
    .map((line) => line.match(/^\s*(?:selected project|project name)\s*[:\uFF1A-]\s*(.{1,64})$/i))
    .find(Boolean);
  if (inlineProject) project = safeText(inlineProject[1]);
  if (!project) {
    const projectIndex = lines.findIndex((line) => /^project$/i.test(line));
    if (projectIndex >= 0) project = safeText(lines[projectIndex + 1]);
  }
  if (project && /\b(?:billing|account)\b/i.test(project)) project = null;

  return {
    used: spend.amount,
    cap_used: capUsed ? capUsed.amount : null,
    limit: limit ? limit.amount : null,
    currency: spend.currency,
    period,
    resets_at: null,
    scope: project ? `Project ${project}` : null,
    source: 'dom'
  };
})()
'@

function Extract-Target([System.Net.WebSockets.ClientWebSocket]$ws, [System.Threading.CancellationTokenSource]$cts, [string]$targetId, [int]$pollCount) {
    $script:nextId += 1
    $attachId = $script:nextId
    Send-Cdp $ws $cts $attachId 'Target.attachToTarget' @{ targetId = $targetId; flatten = $true }
    do { $message = Receive-Cdp $ws $cts } until ($message.id -eq $attachId)
    if ($message.error) { return $null }
    $sessionId = [string]$message.result.sessionId
    if (-not $sessionId) { return $null }
    try {
        for ($attempt = 0; $attempt -lt $pollCount; $attempt += 1) {
            $script:nextId += 1
            $evaluateId = $script:nextId
            Send-Cdp $ws $cts $evaluateId 'Runtime.evaluate' @{
                expression = $extractExpression
                returnByValue = $true
                awaitPromise = $false
            } $sessionId
            do { $message = Receive-Cdp $ws $cts } until ($message.id -eq $evaluateId)
            if (-not $message.error -and $null -ne $message.result.result.value) {
                return $message.result.result.value
            }
            Start-Sleep -Milliseconds 500
        }
        return $null
    } finally {
        try {
            $script:nextId += 1
            Send-Cdp $ws $cts $script:nextId 'Target.detachFromTarget' @{ sessionId = $sessionId }
        } catch {}
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
        $endpoints += [pscustomobject]@{
            port = $port
            wsUrl = ("ws://127.0.0.1:{0}{1}" -f $port, $path)
        }
    } catch {}
}

$endpoints = @($endpoints | Group-Object port | ForEach-Object {
    $_.Group | Sort-Object { if ($_.wsUrl) { 0 } else { 1 } } | Select-Object -First 1
})

foreach ($endpoint in $endpoints) {
    $ws = $null
    $cts = $null
    try {
        $listener = Get-NetTCPConnection -State Listen -LocalPort $endpoint.port -ErrorAction Stop |
            Where-Object { $_.LocalAddress -in @('127.0.0.1', '::1') } |
            Select-Object -First 1
        if (-not $listener) { continue }
        $owner = Get-Process -Id $listener.OwningProcess -ErrorAction Stop
        if ($owner.ProcessName -notin @('msedge', 'chrome', 'brave')) { continue }

        $wsUrl = [string]$endpoint.wsUrl
        if (-not $wsUrl) {
            $version = Invoke-RestMethod -Uri ("http://127.0.0.1:{0}/json/version" -f $endpoint.port) -TimeoutSec 2
            $wsUrl = [string]$version.webSocketDebuggerUrl
        }
        if (-not $wsUrl -or $wsUrl -notmatch '^ws://127\.0\.0\.1:[0-9]+/devtools/browser/[A-Za-z0-9._-]+$') { continue }

        $cts = [System.Threading.CancellationTokenSource]::new()
        $cts.CancelAfter($timeoutMs)
        $ws = [System.Net.WebSockets.ClientWebSocket]::new()
        [void]$ws.ConnectAsync([Uri]$wsUrl, $cts.Token).GetAwaiter().GetResult()

        $script:nextId = 1
        Send-Cdp $ws $cts 1 'Target.getTargets' @{}
        do { $message = Receive-Cdp $ws $cts } until ($message.id -eq 1)
        if ($message.error) { continue }
        $targetInfos = @($message.result.targetInfos)

        foreach ($info in $targetInfos) {
            if ($info.type -ne 'page') { continue }
            $url = [string]$info.url
            if ($url -notmatch '^https://aistudio\.google\.com/spend(?:[/?]|$)') { continue }
            $payload = Extract-Target $ws $cts ([string]$info.targetId) 10
            if ($null -ne $payload) {
                [Console]::Out.WriteLine(($payload | ConvertTo-Json -Compress -Depth 8))
                exit 0
            }
        }

        $targetId = $null
        try {
            $script:nextId += 1
            $createId = $script:nextId
            Send-Cdp $ws $cts $createId 'Target.createTarget' @{
                url = 'https://aistudio.google.com/spend'
                hidden = $true
                background = $true
            }
            do { $message = Receive-Cdp $ws $cts } until ($message.id -eq $createId)
            if (-not $message.error) {
                $targetId = [string]$message.result.targetId
            }
            if ($targetId) {
                $payload = Extract-Target $ws $cts $targetId 40
                if ($null -ne $payload) {
                    [Console]::Out.WriteLine(($payload | ConvertTo-Json -Compress -Depth 8))
                    exit 0
                }
            }
        } catch {
            # This browser endpoint is not usable for a background AI Studio read.
        } finally {
            if ($targetId) {
                try {
                    $script:nextId += 1
                    Send-Cdp $ws $cts $script:nextId 'Target.closeTarget' @{ targetId = $targetId }
                } catch {}
            }
        }
    } catch {
        # Try the next locally-debuggable Chromium browser. Do not print browser state.
    } finally {
        if ($ws) { try { $ws.Dispose() } catch {} }
        if ($cts) { try { $cts.Dispose() } catch {} }
    }
}

exit 3
"#;

fn parse_spend_stdout(stdout: &[u8]) -> Result<SpendPayload, ProviderError> {
    String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<SpendPayload>(line.trim()).ok())
        .ok_or(ProviderError::NoCookies)
}

#[cfg(windows)]
fn fetch_spend_sync(timeout_secs: u64) -> Result<SpendPayload, ProviderError> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let timeout_ms = timeout_secs.max(3).saturating_mul(1000).min(30_000);
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            AI_STUDIO_CDP_SPEND_SCRIPT,
        ])
        .env("CODEXBAR_GEMINI_API_CDP_TIMEOUT_MS", timeout_ms.to_string())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| ProviderError::Other(format!("Failed to query AI Studio CDP: {error}")))?;
    if !output.status.success() {
        return Err(ProviderError::NoCookies);
    }
    parse_spend_stdout(&output.stdout)
}

#[cfg(not(windows))]
fn fetch_spend_sync(_timeout_secs: u64) -> Result<SpendPayload, ProviderError> {
    Err(ProviderError::NoCookies)
}

pub(super) async fn fetch_spend(timeout_secs: u64) -> Result<SpendPayload, ProviderError> {
    tokio::task::spawn_blocking(move || fetch_spend_sync(timeout_secs))
        .await
        .map_err(|error| ProviderError::Other(format!("Gemini API CDP task failed: {error}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_sanitized_spend_payload_stdout() {
        let payload = parse_spend_stdout(
            br#"{"used":44.75,"cap_used":1054.0,"limit":2000.0,"currency":"JPY","period":"August 7 - September 3, 2026","resets_at":null,"scope":"Project Gemini Project","source":"dom"}
"#,
        )
        .unwrap();
        assert_eq!(payload.used, 44.75);
        assert_eq!(payload.cap_used, Some(1054.0));
        assert_eq!(payload.limit, Some(2000.0));
        assert_eq!(payload.currency, "JPY");
        assert_eq!(payload.scope.as_deref(), Some("Project Gemini Project"));
    }

    #[test]
    fn cdp_capture_is_ascii_and_never_extracts_browser_auth_material() {
        assert!(AI_STUDIO_CDP_SPEND_SCRIPT.is_ascii());
        assert!(!AI_STUDIO_CDP_SPEND_SCRIPT.contains("document.cookie"));
        assert!(!AI_STUDIO_CDP_SPEND_SCRIPT.contains("localStorage"));
        assert!(!AI_STUDIO_CDP_SPEND_SCRIPT.contains("Authorization"));
        assert!(!AI_STUDIO_CDP_SPEND_SCRIPT.contains("Network.getResponseBody"));
        assert!(AI_STUDIO_CDP_SPEND_SCRIPT.contains("Runtime.evaluate"));
        assert!(AI_STUDIO_CDP_SPEND_SCRIPT.contains("DevToolsActivePort"));
        assert!(AI_STUDIO_CDP_SPEND_SCRIPT.contains("Get-NetTCPConnection"));
        assert!(AI_STUDIO_CDP_SPEND_SCRIPT.contains("document.body?.innerText"));
        assert!(AI_STUDIO_CDP_SPEND_SCRIPT.contains("\\u7dcf\\u8cbb\\u7528"));
        assert!(AI_STUDIO_CDP_SPEND_SCRIPT.contains("\\uffe5"));
    }
}
