# Providers (Windows)

Windows rewrite of the *role* of upstream `docs/providers.md`: how providers are registered and fetched in **this** repo.
Do **not** treat upstream’s full strategy table as authoritative for Win-CodexBar without checking code — IDs and auto-order drift.

## Single factory

All shells and the CLI construct providers through:

```text
codexbar::core::instantiate_provider  →  rust/src/core/provider_factory.rs
```

`ProviderId` lives in `rust/src/core/provider.rs`. The factory match is **exhaustive** (missing arm = compile error). Tests ensure every id instantiates.

**Never** duplicate provider factories in the Tauri shell or ad-hoc commands.

## Adding a provider

1. Add a `ProviderId` variant + `cli_name` / `display_name` / cookie domain / `from_cli_name` metadata as required.
2. Implement `Provider` in `rust/src/providers/<name>/` (or module).
3. Add the match arm in `provider_factory.rs::instantiate`.
4. Keep provider-specific parsing and auth **inside** that module — no cross-provider branching in shared UI paths.
5. Keep identity / plan / email **siloed** per provider in the UI.

## Fetch strategies (concept)

Same vocabulary as upstream, implemented in Rust:

| Source label | Meaning (typical) |
|--------------|-------------------|
| `auto` | Provider-specific fallback order |
| `web` | Cookie / dashboard HTTP |
| `cli` | Local CLI / PTY / RPC helpers |
| `oauth` | OAuth-backed flows where supported |

CLI: `codexbar usage --source auto|web|cli|oauth`.

Auth resolution helpers in `rust/src/providers/` commonly try: explicit settings → keyring/entry → environment variables (exact order is provider-specific).

## Cookie-backed providers

Windows browser import: Chrome, Edge, Brave (DPAPI + AES-GCM), Firefox (SQLite).  
Settings → **Providers** → provider detail → choose browser → Import.  
Manual cookie header paste is the fallback (required under WSL for Chromium DPAPI).  
Details: [COOKIES.md](./COOKIES.md).

## Listing what is enabled

```powershell
codexbar config providers
codexbar config enable -p cursor
codexbar config disable -p cursor
```

Desktop: Settings → Providers (sidebar reorder, per-provider credential UI).

## Status pages

Optional status polling (provider status pages) is available via CLI `--status` and Settings advanced toggles where wired. Mapping of Statuspage vs Google incidents is provider metadata in code — see provider modules rather than upstream-only URLs if they disagree.

## Usage & Spend

Desktop tab id: `usageSpend`. The desktop and Overview consume one shared spend catalog. Codex and Claude local logs are first-class; routed OpenCodex usage enriches the matching Codex, OpenCode Go, Kimi, or DeepSeek subscription instead of appearing as a second fake provider. xAI and OpenRouter can publish exact provider-metered daily USD spend when their management credentials are configured, while Grok local sessions contribute tokens only. Missing spend sources remain unknown rather than becoming a false `$0`. Do not invent cross-currency totals.

### AWS Bedrock monitoring

AWS Bedrock is a Windows provider backed by signed Cost Explorer requests and optional CloudWatch activity. It is disabled by default, and monitoring requests can add charges to your AWS bill. AWS currently charges $0.01 per Cost Explorer API request; paginated monthly-spend reads can therefore use more than one billed request, while optional CloudWatch activity is billed under CloudWatch pricing.

The shared refresh interval controls automatic provider polling. `0` / Manual disables the recurring timer, but explicit refreshes and **Refresh when the menu opens** can still fetch Bedrock data. Disable Bedrock itself to stop its app refreshes.

`CODEXBAR_BEDROCK_BUDGET` changes only the displayed monthly progress. It does **not** cap AWS charges, stop polling, or enforce a billing limit.

Custom pricing overlays are exact-match overrides used only where the local spend contract has matching provider/model token evidence. Explicit zero rates mean free; omitted rate fields stay unknown. The Usage & Spend surface keeps provenance/coverage visible, preserves cost-only model rows when token coverage is partial, and can Copy JSON or save the same JSON contract through the native file picker.

### OpenCode, Codex quota, and local cost boundaries

OpenCode-held OpenAI/Codex OAuth can be reused for **remote Codex account quota** only when the Codex provider's `External OAuth sources` setting is explicitly enabled. Native Codex credentials still take precedence, an explicit `CODEX_HOME` stays isolated, and external credentials remain read-only. This does **not** import ordinary OpenCode sessions into Codex token or spend totals. OpenCode Go's local SQLite reader remains scoped to its own `opencode-go` assistant records; OpenAI API-platform usage is a separate provider.

### z.ai Coding Plan quotas

z.ai Coding Plans accept both `TOKENS_LIMIT` and `CREDIT_LIMIT` rows. The shortest known Coding Plan window becomes primary and the longest becomes secondary; `TIME_LIMIT` is the separate MCP lane. When absolute usage/remaining counts are available they determine the used percentage, otherwise the provider percentage is used, always clamped to 0–100%. This behavior is shared by the tray, provider detail, CLI, and other Windows surfaces.

Upstream's independent **WidgetKit** provider-widget configuration has no Windows analogue in this repository. Win-CodexBar has no WidgetKit extension; provider cards and tray entries are already independent Windows/Tauri surfaces.
## Token-based providers (summary endpoints)

### Gemini Apps

- Provider id: `geminiapps` (aliases `gemini-apps`, `gemini-web`). This is intentionally separate from `gemini`, which remains Gemini CLI / Code Assist quota.
- Source: the local Gemini Apps Browser Bridge cache. Authentication, Google cookies, WIZ page state, and raw RPC bodies stay inside the signed-in `gemini.google.com` tab; CodexBar reads only sanitized Current/Weekly percentages, reset times, plan, account slot, parser source, and observation time.
- `Current usage` maps to the primary 5-hour window and `Weekly limit` maps to the secondary 7-day window. The provider accepts only `Auto` / `Web` source modes.
- The bridge normally refreshes an existing Gemini tab every three minutes without activating it or moving mouse/keyboard focus. If no Gemini tab exists, it may create one managed Usage tab with `active: false`; a sign-in redirect is retained instead of creating repeated login tabs. CodexBar preserves the bridge's real `observed_at` timestamp, so the Providers UI marks data older than 10 minutes as stale. Snapshots older than 24 hours fail closed rather than presenting very old consumer quota as usable data.
- Stable cache: `%LOCALAPPDATA%\\CodexBar\\gemini-apps-browser.json`. During the PoC-to-provider transition, the previous `gemini-web-bridge-poc.json` filename remains a read-only compatibility fallback.
- Dashboard: `https://gemini.google.com/usage`. The provider is disabled by default until the Browser Bridge is configured.

### OrcaRouter

- Provider id: `orcarouter` (aliases `orca`, `orca-router`). Base: `https://api.orcarouter.ai/v1`.
- Auth: Bearer API key — Settings → Providers, token accounts, or the `ORCAROUTER_API_KEY` environment variable.
- Reports **workspace-level** usage/subscription summaries only: `GET /v1/dashboard/billing/usage` (`total_usage`) and `GET /v1/dashboard/billing/subscription` (`has_payment_method`, `soft_limit_usd`, `hard_limit_usd`, `system_hard_limit_usd`, `access_until`). Never label these as per-key spend.
- Browser wallet balance is optional enrichment: `GET https://www.orcarouter.ai/api/user/self` supplies `active_workspace.wallet_quota`, converted with the current public `GET /api/status` `quota_per_unit` value instead of a hard-coded rate. Automatic mode first uses a locally exposed Chromium DevTools session and reads only the browser's `/api/user/self` response body (no auth token extraction). On current Edge/Chrome, the official remote-debugging/agent auto-connect flow exposes this through `DevToolsActivePort`; CodexBar accepts that endpoint only while its loopback listener is live and owned by a Chromium process. Browser-cookie import remains the fallback. Chromium App-Bound Encryption (ABE) can block automatic cookie decryption, so manual cookie input remains supported.
- `Auto` prefers the API workspace summary and adds browser wallet balance when a readable logged-in session exists. If the API path is unavailable but the browser wallet succeeds, Auto degrades to the wallet-only Web result. `OAuth` is API-only; `Web` is wallet-only.
- Spend and prepaid wallet balance remain separate fields in the UI. A missing browser session never turns a successful API usage result into an error.
- Missing/null summary fields surface as unknown — never as fabricated zeroes. Historical billing is not implemented (endpoints are summary-only).

## Upstream doc warning

Upstream `docs/providers.md` is a large auto-strategy matrix (60+ providers) for the macOS app. Use it as **inspiration** when porting a provider. For runtime truth on Windows:

1. `rust/src/core/provider.rs` (`ProviderId`)
2. `rust/src/providers/<id>/`
3. `codexbar usage -p <id> -v` / desktop provider detail errors

## Related

- [ARCHITECTURE.md](./ARCHITECTURE.md)
- [CLI.md](./CLI.md)
- [CONFIGURATION.md](./CONFIGURATION.md)
- [COOKIES.md](./COOKIES.md)
