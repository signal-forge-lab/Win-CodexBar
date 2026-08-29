# Gemini Apps Browser Bridge: source audit, CodexBar design, and PoC

Date: 2026-08-29

## Goal

Add the consumer Gemini Apps usage meter (`gemini.google.com/usage`) to Win-CodexBar without mixing it with the existing Gemini CLI / Code Assist quota and without copying Google cookies or page tokens into CodexBar.

This document records the source audit and the first implementation shape. The production provider is intentionally not added until the browser bridge has been proven against a real signed-in Gemini Apps account.

## Reference implementations audited

### AI Quota Deck

Repository: `JoshuaWang2211/ai-quota-deck`

Audited revision: `d810abdd5b0df8d25d333bfcc483b908e488c3d5`

License: MIT.

Important files:

- `browser-bridge/manifest.json`
- `browser-bridge/src/background.js`
- `browser-bridge/src/gemini-interceptor.js`
- `browser-bridge/src/gemini-parser.js`
- `browser-bridge/src/gemini.js`
- `src-tauri/src/native_host.rs`
- `src-tauri/src/gemini.rs`

Observed design:

1. A `MAIN`-world content script reads `window.WIZ_global_data.SNlM0e`.
2. The token is relayed only inside the Gemini tab with `window.postMessage`.
3. An isolated content script calls a same-origin Gemini `batchexecute` RPC (`jSf9Qc`).
4. Only quota values, reset times, account slot, tier, and observation time are sent to the extension background worker.
5. The background worker validates the sender URL and frame, then forwards the sanitized payload through Chromium Native Messaging.
6. The Rust native host pins allowed extension origins and stores an atomic local cache.
7. The desktop app reads that cache, distinguishes fresh vs stale data, and never receives the Google page token or cookies.

Strengths worth carrying into CodexBar:

- Strong credential boundary: cookies and WIZ tokens stay in the browser tab.
- Native Messaging avoids exposing a writable unauthenticated localhost HTTP endpoint.
- Exact extension-origin allowlist in the native host manifest.
- Small typed wire payload and bounded frame size.
- Explicit stale-cache behavior.
- Multi-account path support through `/u/<N>/...` account slots.
- Refresh on browser lifecycle / alarms rather than browser database scraping.
- Background refresh does not activate the Gemini tab or steal user focus.

Weaknesses / risks:

- The Gemini RPC is undocumented and may change without notice.
- The parser is shape-dependent.
- The extension must remain installed and a signed-in Gemini tab must be available for fresh readings.
- The current implementation maps internal tier codes directly; those codes are not a stable public contract.

### Riah Usage

Repository: `RiahStudio/riah-usage`

Audited revision: `748a55099646a2b77fc3efa5a276160dbaa94838`

License: MIT.

Important files:

- `gemini-sync.js`
- `sync-gemini.js`
- `gemini-capture.html`
- `docs/providers.md`

Observed design:

1. Runs on `gemini.google.com/usage` in the already-authenticated browser.
2. Reads page-state tokens (`SNlM0e`, build/session values) and calls same-origin `batchexecute` RPC `VxUbXb`.
3. Decodes `Current usage` and `Weekly limit`, including reset timestamps and plan code.
4. Falls back to visible DOM text when the RPC parser cannot find the expected shape.
5. Sends only plan + meter percentages + reset times to the local dashboard.

Strengths worth carrying into CodexBar:

- A second independently implemented Gemini Apps RPC gives us a compatibility fallback.
- DOM fallback is useful as a degraded-mode probe when internal RPC shape changes.
- Explicit `Current usage` / `Weekly limit` terminology matches the consumer Gemini usage page.

Weaknesses / risks:

- The bookmarklet path posts to a localhost HTTP endpoint; that is broader than necessary for CodexBar.
- DOM regex parsing is presentation-dependent and must remain last-resort only.
- Some helper paths attempt browser-cookie extraction. CodexBar should not use those paths for Gemini Apps because Windows Chromium ABE and the sensitivity of Google session cookies make that a worse security boundary.

## Decision

Use an AI-Quota-Deck-style Native Messaging boundary, with parser redundancy inspired by both projects.

The bridge owns all page-authenticated work:

```text
gemini.google.com/usage
  MAIN world: read WIZ page state
        |
        | window.postMessage (tab-local only)
        v
  isolated content script
        |
        | same-origin batchexecute
        | 1. jSf9Qc
        | 2. VxUbXb fallback
        | 3. DOM fallback (last resort)
        v
  sanitized quota DTO
        |
        v
  extension background
        |
        | Chrome Native Messaging
        v
  CodexBar native host
        |
        v
  %LOCALAPPDATA%/CodexBar/gemini-apps-browser.json
```

The following must never cross the browser/native boundary:

- Cookie headers / cookie values
- `SNlM0e`
- `FdrFJe`
- build/session page tokens
- Authorization headers
- raw Gemini HTML
- raw RPC response bodies

## PoC wire contract

```json
{
  "version": 1,
  "provider": "gemini-apps",
  "observed_at": 1787990400,
  "payload": {
    "account_id": "0",
    "plan": "Pro",
    "source": "jSf9Qc",
    "current": {
      "label": "Current usage",
      "used_percent": 25.0,
      "resets_at": "2026-08-29T12:00:00Z"
    },
    "weekly": {
      "label": "Weekly limit",
      "used_percent": 10.0,
      "resets_at": "2026-09-03T12:00:00Z"
    }
  }
}
```

The native host uses `serde(deny_unknown_fields)` and rejects malformed percentages, invalid account slots, invalid timestamps, unknown sources, and any shape that attempts to smuggle an additional token/cookie field.

## PoC code layout

```text
tools/gemini-web-bridge-poc/
  README.md
  extension/
    manifest.json
    background.js
    gemini-interceptor.js
    gemini-parser.js
    gemini-content.js
  host/
    install-native-host.ps1
  tests/
    parser.test.js

rust/src/bin/
  codexbar-gemini-web-bridge-poc.rs
```

The PoC is deliberately isolated from `ProviderId` and the normal refresh engine. A successful live capture is required before production integration.

## Production CodexBar design after PoC proof

Add a new provider rather than changing the existing Gemini provider:

```text
Gemini                 -> existing CLI / Code Assist quota
Gemini Apps            -> consumer gemini.google.com/usage quota
```

Proposed production layout:

```text
rust/src/providers/geminiapps/
  mod.rs              Provider implementation
  bridge_cache.rs     typed cache read / freshness rules
  wire.rs             sanitized Browser Bridge DTO

apps/desktop-tauri/src-tauri/src/
  browser_bridge.rs   native-host registration + cache receiver mode

apps/desktop-tauri/src/
  existing provider-card / float-bar paths (no special UI branch expected)

resources/browser-bridge/gemini-apps/
  extension files staged beside the installed app
```

Proposed `ProviderId` metadata:

- CLI name: `geminiapps`
- Display name: `Gemini Apps`
- Primary label: `Current usage`
- Secondary label: `Weekly limit`
- Dashboard URL: `https://gemini.google.com/usage`
- Source: browser bridge / web
- Default enabled: false until bridge is configured

The existing Gemini CLI provider remains untouched and keeps reporting Code Assist / CLI quota.

## Implementation sequence

### Phase 0 — audit and contract

- [x] Pin and inspect AI Quota Deck reference source.
- [x] Pin and inspect Riah Usage reference source.
- [x] Record trust boundaries and failure modes.
- [x] Define a secret-free typed wire contract.

### Phase 1 — isolated PoC

- [x] Add Gemini-only MV3 extension scaffold with declared Native Messaging permission.
- [x] Add non-foreground 3-minute refresh alarm and Memory Saver recovery for existing Gemini tabs.
- [x] Keep WIZ tokens in the page and perform same-origin RPC fetches in the content script.
- [x] Add `jSf9Qc` parser.
- [x] Add `VxUbXb` fallback parser.
- [x] Add DOM fallback parser.
- [x] Add native-host receiver with strict DTO validation.
- [x] Add Windows native-host registration helper.
- [x] Add deterministic parser/host tests.

### Phase 2 — live proof

- [x] Build `codexbar-gemini-web-bridge-poc.exe`.
- [x] Load the unpacked extension in the user's signed-in Chromium browser.
- [x] Register the native host for the actual unpacked extension ID.
- [x] Open `https://gemini.google.com/usage` with the intended account.
- [x] Verify cache contains only the approved DTO fields.
- [x] Record which RPC path succeeded (`jSf9Qc`, `VxUbXb`, or DOM).
- [ ] Optional visual cross-check against the foreground Gemini Usage page. This
      was intentionally not performed during the PoC because the user required
      the bridge and validation flow not to steal cursor/focus. It is not needed
      to establish the Browser -> Native Messaging -> CodexBar data path.

Live proof on 2026-08-29 used Microsoft Edge and a signed-in Gemini Apps Usage page.
The first-choice `jSf9Qc` path succeeded. The bridge cache reported a Pro plan,
Current `0.0%`, Weekly `0.005576%`, and concrete UTC reset timestamps. A later
background refresh reproduced the same usage values with a newer `observed_at`,
which proves the bridge was not only replaying the initial push. The `jSf9Qc`
ratio semantics were also cross-checked against the audited AI Quota Deck source,
where the same field is treated as allowance consumed (used percentage).

No foreground/page-visual comparison is claimed here. Microsoft Edge did not
have CDP enabled, and validation deliberately did not activate the Gemini tab or
use mouse/keyboard automation merely to obtain a screenshot.

The live cache schema was inspected after capture:

```text
root:    version, provider, observed_at, payload
payload: account_id, plan, source, current, weekly
meter:   label, used_percent, resets_at
```

No cookie, token, authorization, WIZ-state, HTML, or raw-response field was
present. The native host process was also observed with only the expected
Chromium extension origin and Chromium `--parent-window` argument.

### Phase 3 — production provider

- [x] Add `ProviderId::GeminiApps` and factory wiring.
- [x] Move the typed cache reader into `rust/src/providers/geminiapps/`.
- [x] Convert Current / Weekly into normal `RateWindow`s.
- [x] Add stale/fresh behavior and account-slot validation.
- [ ] Add provider settings/onboarding for installing the Browser Bridge.
- [ ] Stage the extension during the desktop build/install flow.
- [x] Pass the existing dashboard / float-bar / tray regression suites with `Gemini Apps` in the provider catalog.

### Phase 4 — hardening

- [ ] Decide account pinning UX for multiple `/u/N` sessions.
- [ ] Add parser fixture probes for future Gemini response changes.
- [x] Fail closed when only a partial quota response is available.
- [x] Never fall back to browser-cookie database extraction automatically.
- [ ] Preserve a last-known-good snapshot with an explicit stale marker.

## Acceptance rule for production

The PoC acceptance rule is satisfied: a real browser session produced both
usage windows through `jSf9Qc`, repeated background refreshes updated the cache,
and the cache was inspected for secret leakage. The production `geminiapps`
provider is now wired into the provider catalog and reads that sanitized cache.
Dedicated onboarding and installer staging for the Browser Bridge remain future
packaging work; they are not required for the provider runtime itself. The
undocumented RPCs remain a compatibility surface with fail-closed parsing and
fixtures. A future visual cross-check may be performed when it can be done
without violating the non-foreground-operation requirement; it is not evidence
required for the transport or provider-runtime acceptance above.
