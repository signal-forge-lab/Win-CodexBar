param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[a-p]{32}$')]
    [string]$ExtensionId,

    [string]$HostExe,

    [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$HostName = 'com.codexbar.gemini_web_bridge_poc'
$Root = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..\..'))
$StateDir = Join-Path $env:LOCALAPPDATA 'CodexBar\gemini-web-bridge-poc'
$ManifestPath = Join-Path $StateDir "$HostName.json"
$OriginsPath = Join-Path $StateDir 'allowed-origins.txt'
$LegacyOriginPath = Join-Path $StateDir 'allowed-origin.txt'
$Origin = "chrome-extension://$ExtensionId/"

$BrowserRoots = @(
    'Software\Google\Chrome',
    'Software\Microsoft\Edge',
    'Software\BraveSoftware\Brave-Browser',
    'Software\Vivaldi',
    'Software\Chromium'
)

function NativeHostKey([string]$BrowserRoot) {
    return "HKCU:\$BrowserRoot\NativeMessagingHosts\$HostName"
}

if ($Uninstall) {
    foreach ($browserRoot in $BrowserRoots) {
        $key = NativeHostKey $browserRoot
        if (Test-Path $key) {
            Remove-Item $key -Recurse -Force
        }
    }
    if (Test-Path $ManifestPath) { Remove-Item $ManifestPath -Force }
    if (Test-Path $OriginsPath) { Remove-Item $OriginsPath -Force }
    if (Test-Path $LegacyOriginPath) { Remove-Item $LegacyOriginPath -Force }
    Write-Host 'Removed CodexBar Gemini Apps PoC Native Messaging registration.'
    exit 0
}

if (-not $HostExe) {
    $ReleaseHost = Join-Path $Root 'target\release\codexbar-gemini-web-bridge-poc.exe'
    $DebugHost = Join-Path $Root 'target\debug\codexbar-gemini-web-bridge-poc.exe'
    $HostExe = if (Test-Path $ReleaseHost -PathType Leaf) { $ReleaseHost } else { $DebugHost }
}
$HostExe = [System.IO.Path]::GetFullPath($HostExe)
if (-not (Test-Path $HostExe -PathType Leaf)) {
    throw "Native host executable not found: $HostExe. Build it first with cargo build --manifest-path rust\Cargo.toml --bin codexbar-gemini-web-bridge-poc"
}

New-Item -ItemType Directory -Path $StateDir -Force | Out-Null
$origins = @()
if (Test-Path $OriginsPath) {
    $origins += Get-Content -LiteralPath $OriginsPath | ForEach-Object { $_.Trim() } | Where-Object { $_ }
} elseif (Test-Path $LegacyOriginPath) {
    $origins += (Get-Content -LiteralPath $LegacyOriginPath -Raw).Trim()
}
if (Test-Path $ManifestPath) {
    try {
        $existingManifest = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
        $origins += @($existingManifest.allowed_origins)
    } catch {
        # Rebuild a malformed/legacy manifest from the validated caller origin.
    }
}
$origins += $Origin
$origins = @($origins | Where-Object { $_ -match '^chrome-extension://[a-p]{32}/$' } | Sort-Object -Unique)
$origins | Set-Content -LiteralPath $OriginsPath -Encoding ASCII
# Compatibility for an older native host binary during an in-place upgrade.
$Origin | Set-Content -LiteralPath $LegacyOriginPath -Encoding ASCII -NoNewline

$manifest = [ordered]@{
    name = $HostName
    description = 'CodexBar Gemini Apps Browser Bridge'
    path = $HostExe
    type = 'stdio'
    allowed_origins = $origins
}
$manifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $ManifestPath -Encoding UTF8

$registered = @()
foreach ($browserRoot in $BrowserRoots) {
    $rootKey = "HKCU:\$browserRoot"
    if (-not (Test-Path $rootKey)) { continue }
    $key = NativeHostKey $browserRoot
    New-Item -Path $key -Force | Out-Null
    Set-ItemProperty -Path $key -Name '(default)' -Value $ManifestPath
    $registered += $browserRoot
}

if ($registered.Count -eq 0) {
    throw 'No supported Chromium browser registry root was found.'
}

Write-Host "Registered Native Messaging host for $Origin"
Write-Host "Allowed extension origins: $($origins.Count)"
$origins | ForEach-Object { Write-Host "  $_" }
Write-Host "Manifest: $ManifestPath"
Write-Host "Host: $HostExe"
Write-Host 'Restart the browser after changing Native Messaging registration.'
