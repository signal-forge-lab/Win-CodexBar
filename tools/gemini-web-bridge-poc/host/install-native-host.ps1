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
$OriginPath = Join-Path $StateDir 'allowed-origin.txt'
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
    if (Test-Path $OriginPath) { Remove-Item $OriginPath -Force }
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
$Origin | Set-Content -LiteralPath $OriginPath -Encoding ASCII -NoNewline

$manifest = [ordered]@{
    name = $HostName
    description = 'CodexBar Gemini Apps Browser Bridge'
    path = $HostExe
    type = 'stdio'
    allowed_origins = @($Origin)
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
Write-Host "Manifest: $ManifestPath"
Write-Host "Host: $HostExe"
Write-Host 'Restart the browser after changing Native Messaging registration.'
