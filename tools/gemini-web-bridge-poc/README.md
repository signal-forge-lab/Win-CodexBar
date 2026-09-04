# CodexBar Browser Bridge

This bridge reads `gemini.google.com/usage`, the signed-in AI Studio `/spend`
page, and AIHubMix's signed-in `/topup` Transactions page without exporting
cookies, authorization headers, Clerk JWTs, or page tokens from the browser.
It started as an isolated Gemini PoC and is now consumed by `geminiapps`,
`gemini-api`, and `aihubmix`. The extension is shown as **CodexBar Browser
Bridge** in Chromium extension managers.

The existing `gemini` provider remains separate and continues to report Gemini CLI / Code Assist quota.

## Data flow

```text
Gemini Usage page
  -> same-origin Gemini RPC inside the tab
  -> sanitized Current / Weekly DTO
  -> Chrome Native Messaging
  -> codexbar-gemini-web-bridge-poc.exe
  -> %LOCALAPPDATA%\CodexBar\gemini-apps-browser.json
  -> GeminiAppsProvider (`geminiapps`)

AI Studio Spend page
  -> sanitized current-period spend DTO
  -> Chrome Native Messaging
  -> codexbar-gemini-web-bridge-poc.exe
  -> %LOCALAPPDATA%\CodexBar\gemini-api-spend-browser.json
  -> GeminiApiProvider (`gemini-api`)

AIHubMix Topup / Transactions page
  -> page-owned `/call/usr/quota_rec` response or visible table
  -> sanitized latest funded `balance_after` amount only
  -> Chrome Native Messaging
  -> codexbar-gemini-web-bridge-poc.exe
  -> %LOCALAPPDATA%\CodexBar\aihubmix-recharge-browser.json
  -> AiHubMixProvider (`aihubmix`) + live Manage-Key current balance
```

The cache may contain only:

- Gemini account slot (`/u/N`)
- plan label when known
- parser source name
- Current usage percentage + reset time
- Weekly limit percentage + reset time
- observation time

Cookies, WIZ page tokens, raw HTML, and raw RPC payloads are rejected by the host's typed contract.

The Gemini API spend cache contains only net `Total cost`, the current amount
shown in the Monthly spend cap panel, the configured cap when available,
currency, period/reset metadata when present, and a sanitized project label.
The cap panel is interpreted as `current / cap`; its current amount is never
mistaken for the configured cap. The provider deliberately does not invent a
quota percentage when AI Studio exposes no configured cap.

The AIHubMix recharge cache contains only the latest funded balance in USD,
an optional funding timestamp, parser source, and observation time. The bridge
does not export the Clerk JWT, Manage Key, access token, cookie header, email,
raw Transactions response, or individual transaction records.

## Build the native host

From the repository root:

```powershell
cargo build -p codexbar --release --bin codexbar-gemini-web-bridge-poc
```

The binary is written to:

```text
target\release\codexbar-gemini-web-bridge-poc.exe
```

## Load the extension

1. Open `edge://extensions` or `chrome://extensions`.
2. Enable Developer mode.
3. Choose **Load unpacked**.
4. Select `tools\gemini-web-bridge-poc\extension`.
5. Copy the resulting extension ID.

The selected folder itself must contain `manifest.json`; do not select the
repository root or the parent `tools\gemini-web-bridge-poc` directory.

Repeat **Load unpacked** for every Chrome/Edge profile that may hold the signed-in Gemini account you want CodexBar to read. If a profile reports a different extension ID, register that ID too; the installer merges allowed origins instead of replacing the previously registered Edge/Chrome IDs.

The PoC intentionally does not ship a fixed manifest key. This prevents us from pretending an unpacked development ID is a production identity.

## Register Native Messaging

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\gemini-web-bridge-poc\host\install-native-host.ps1 `
  -ExtensionId <the-extension-id>
```

Run the same command once for each distinct extension ID shown by your Chromium profiles. Existing registered IDs are preserved.

This writes a user-scoped Native Messaging manifest and registry entry. It prefers the release host when present and falls back to a debug build for development. No admin rights are required.

To uninstall the registration:

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\gemini-web-bridge-poc\host\install-native-host.ps1 `
  -ExtensionId <the-extension-id> -Uninstall
```

## Capture a real reading

1. Open `https://gemini.google.com/usage` for Gemini Apps quota,
   `https://aistudio.google.com/spend` for Gemini API spend, and/or
   `https://console.aihubmix.com/topup` for AIHubMix recharge history in the
   account you want to measure.
2. Keep the Usage tab open for the first capture. `nativeMessaging` is declared
   by the PoC extension, while the native host itself still accepts only the
   exact unpacked extension origin registered by `install-native-host.ps1`.
3. Inspect:

```text
%LOCALAPPDATA%\CodexBar\gemini-apps-browser.json
%LOCALAPPDATA%\CodexBar\gemini-api-spend-browser.json
%LOCALAPPDATA%\CodexBar\aihubmix-recharge-browser.json
```

Expected shape is documented in `docs/experiments/gemini-apps-browser-bridge-poc.md`.

## Deterministic tests

Parser tests use synthetic, secret-free fixtures:

```powershell
node --test tools\gemini-web-bridge-poc\tests\parser.test.js
cargo test -p codexbar --bin codexbar-gemini-web-bridge-poc
```

## Bridge limitations

- Gemini's internal RPCs are undocumented.
- Fresh readings require signed-in Gemini / AI Studio browser sessions.
  AIHubMix recharge capture likewise requires an already-open signed-in Topup
  tab. If no matching tab exists, the bridge does nothing and never creates one
  on its own. The bridge never activates an AIHubMix tab, reloads it, or moves
  keyboard/mouse focus. Gemini / AI Studio retain their existing refresh logic.
- CodexBar's Gemini API provider consumes only this Browser Bridge cache during
  provider refreshes and does not initiate a DevTools/CDP connection itself.
- CodexBar's AIHubMix Auto mode uses the Manage Key only for current balance and
  the secret-free Browser Bridge cache for the funded-balance anchor. It never
  calls `/api/user/token`, never calls the Clerk-protected Transactions endpoint
  from the native provider, and never initiates CDP. Explicit `Web` mode remains
  a separate opt-in browser/CDP balance path.
- The AI Studio DOM parser accepts the current English/Japanese Spend layouts,
  including split label/value rows. It records net `Total cost` / `総費用`, not
  pre-discount charges, and leaves the spend cap unknown when the UI only offers
  the action to configure one. Reset timestamps are not inferred from localized text.
- Multi-account selection is not a dedicated UX yet; the payload records the `/u/N` slot so account pinning can be added later.
