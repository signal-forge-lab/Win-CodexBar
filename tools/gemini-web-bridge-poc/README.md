# Gemini Apps Browser Bridge

This bridge reads `gemini.google.com/usage` without exporting Google cookies or page tokens from the browser. It started as an isolated PoC and is now consumed by CodexBar's `geminiapps` provider.

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
```

The cache may contain only:

- Gemini account slot (`/u/N`)
- plan label when known
- parser source name
- Current usage percentage + reset time
- Weekly limit percentage + reset time
- observation time

Cookies, WIZ page tokens, raw HTML, and raw RPC payloads are rejected by the host's typed contract.

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

The PoC intentionally does not ship a fixed manifest key. This prevents us from pretending an unpacked development ID is a production identity.

## Register Native Messaging

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\gemini-web-bridge-poc\host\install-native-host.ps1 `
  -ExtensionId <the-extension-id>
```

This writes a user-scoped Native Messaging manifest and registry entry. It prefers the release host when present and falls back to a debug build for development. No admin rights are required.

To uninstall the registration:

```powershell
powershell -ExecutionPolicy Bypass -File .\tools\gemini-web-bridge-poc\host\install-native-host.ps1 `
  -ExtensionId <the-extension-id> -Uninstall
```

## Capture a real reading

1. Open `https://gemini.google.com/usage` in the account you want to measure.
2. Keep the Usage tab open for the first capture. `nativeMessaging` is declared
   by the PoC extension, while the native host itself still accepts only the
   exact unpacked extension origin registered by `install-native-host.ps1`.
3. Inspect:

```text
%LOCALAPPDATA%\CodexBar\gemini-apps-browser.json
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
- Fresh readings require a signed-in Gemini browser session. Refreshes run
  against background Gemini tabs. If none exists, the bridge creates one
  managed `gemini.google.com/usage` tab with `active: false`; if that tab is
  redirected to sign-in, its tab id is retained so alarms do not create a
  login-tab loop. The bridge never activates a tab or moves keyboard/mouse
  focus. It disables Memory Saver auto-discard for matching tabs and may reload
  a background tab if Chromium already discarded or froze it.
- The DOM parser is deliberately last-resort and does not infer reset timestamps from localized text.
- Multi-account selection is not a dedicated UX yet; the payload records the `/u/N` slot so account pinning can be added later.
