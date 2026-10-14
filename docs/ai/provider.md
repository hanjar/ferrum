# AI-04 — Provider backend, credential and setup

Task: AI-04 / FER-161 (FER-187..FER-190). Branch `feature/ai-a-provider`, base `9a33c2229d6bc160762256165d07c4bee355e482`.
Status: policy layer implemented and tested against fakes. **No real HTTP transport is wired and no live smoke test was run** (no HTTP client crate in `src-tauri/Cargo.toml`, no Tauri command registered; both belong to the integrator).

## Primary sources (checked 2026-10-01)

| Fact                                                                                                                                                                                                                                                                                                                                                                                                                      | Source                                                                                                             |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| Z.ai general API: `POST https://api.z.ai/api/paas/v4/chat/completions`, `Authorization: Bearer <key>`; fields `model`, `messages`, `stream`, `temperature` [0,1], `max_tokens`, `response_format` = `{"type":"json_object"}` or text, `thinking`; response `choices[0].message.content`, `finish_reason` in `stop, tool_calls, length, sensitive, model_context_window_exceeded, network_error`; errors `{code, message}` | https://docs.z.ai/api-reference/llm/chat-completion                                                                |
| Z.ai error codes: 1000/1001/1003 auth (401), 1302 and 1305 rate limit (429), 1113 insufficient balance (429), 1210 invalid parameter (400), 1211 unknown model (400)                                                                                                                                                                                                                                                      | https://docs.z.ai/api-reference/api-code                                                                           |
| GLM Coding Plan uses a different endpoint (`api/coding/paas/v4`) and is "strictly limited" to officially supported tools; it must not be assumed valid for Ferrum                                                                                                                                                                                                                                                         | https://docs.z.ai/devpack/faq                                                                                      |
| OpenAI-compatible: `/chat/completions`, `response_format` `json_object` (JSON mode) or `json_schema` (Structured Outputs), 401/429/500/503 semantics, `insufficient_quota` for exhausted credit                                                                                                                                                                                                                           | https://platform.openai.com/docs/guides/structured-outputs and https://platform.openai.com/docs/guides/error-codes |

Not verified against a live account: model IDs, whether a given account may call the general endpoint, or third-party router behavior. Model ID and endpoint are user configuration (never hardcoded). OpenAI-compatible routers are treated as generic endpoints.

Decisions from the sources:

- Wire format is chat completions with `response_format: json_object` (the only structured mode Z.ai documents). `json_schema` strict mode is intentionally not used: strict schemas force optional `outputName` to be present/null, which `contract.rs` rejects, and the question reply is not part of the envelope schema. Local strict parsing plus one repair request provides the guarantee. `structured: "none"` omits `response_format` for endpoints that reject it.
- The `thinking` parameter is not sent (not part of the generic OpenAI contract). Reasoning models may spend the 2048 token budget; this surfaces as `finish_reason: length` -> repair -> `malformed`, never as a partial plan.

## Modules

- `src-tauri/src/ai/provider.rs`: backend logic (injected `HttpTransport`, `CredentialSource`, `Clock`, `LogSink`).
- `src-tauri/src/ai/credential.rs`: unchanged Windows Credential Manager reader (`Ferrum/AI/custom` only).
- `src/lib/ai/remote-provider.ts`: frontend bridge; sends only config + opaque request; maps backend `{kind}` errors.
- `src/lib/ai/provider-setup.ts`: explicit setup validation, `testConnection`, and `OUTBOUND_DISCLOSURE` text (UI Settings is not part of this task).

## Behavior contract (verified by tests)

- Endpoint: HTTPS, or HTTP only for `localhost`/`127.0.0.1` with explicit port. No userinfo, query, fragment, backslash, `%`, dot segments, IPv6 literal, or bare host. Base path is 1-4 plain segments (`/v1`, `/api/paas/v4`); `/chat/completions` is appended. Rejected before the credential is read or any request is made.
- Config: model `[A-Za-z0-9._:/-]{1,128}`, `timeoutMs` 100..120000 (overall budget including retries), `credentialRef` must be `Ferrum/AI/custom`.
- Outbound: system prompt (capability schema, data-not-instructions), `requestId`, the typed request (<= 8 KiB), and `OutboundContext` (active pane, disk/archive kinds, two counts, hidden and filter flags). `deny_unknown_fields`: no paths, names, listings, contents. The Bearer value is only in `HttpRequest.bearer` (`Secret`, Debug prints `<redacted>`), never in the JSON body.
- Output: only one JSON object (no code fences or prose) that is either `{"kind":"question","text"}` (<= 512 bytes, no control chars) or an `IntentEnvelope` accepted by `contract::parse_intent` for the active request ID. Anything else is malformed.
- Retry: at most 3 attempts per logical request for rate limit (429 except quota), 5xx and network errors; backoff 500 ms, 1 s, ... capped 8 s; `Retry-After` honored but capped 10 s; stops with `timeout` if the next delay would pass the overall deadline. Auth, quota, bad request, model unavailable, refusal and timeout are never retried.
- Malformed/truncated output: exactly one repair request (conversation + the model's own rejected text, truncated, + fixed instruction), then `malformed`. Refusal/`sensitive` is `refused` without repair. Total requests for a plan: <= 2 chains.
- Cancel: `CancelToken` checked before every attempt, during backoff, and after the response; a response that arrives after cancel is dropped (`cancelled`). The module never enqueues or mutates anything, so cancel/timeout/auth/rate-limit/malformed leave queue and filesystem untouched.
- Redaction: diagnostics pass `redact()` (known secret, `Bearer <token>`, `sk-...` keys), single line, <= 300 chars. Error values carry only `kind` and `attempts`; provider bodies are never stored in errors.
- Test connection: explicit, user-triggered; fixed `ping` message, `max_tokens` 8, no user text, request ID or context; at most 2 attempts; returns `{ok, kind}`.

Error kinds (snake_case wire ids, also TS `RemoteProviderError.code`): `invalid_config, unsafe_endpoint, credential, auth, rate_limited, quota_exceeded, model_unavailable, bad_request, server_error, network, timeout, cancelled, malformed, refused` (+ TS-only `backend_failure` for unknown rejections).

## Handoff for the integrator (not edited by this lane)

Needed in files this lane does not own.

1. `src-tauri/src/ai/mod.rs` (new, or add to the existing module declaration): `pub mod contract; pub mod credential; pub mod provider;` and `mod ai;` in `lib.rs`. `provider.rs` uses `super::contract` and `super::credential`.
2. `src-tauri/Cargo.toml`: an HTTP client. Proposal: `ureq = { version = "3", features = ["json"] }` (blocking, rustls; call inside `tauri::async_runtime::spawn_blocking`). Reason: no HTTP crate exists today; the policy layer is transport-agnostic. This is an unverified sketch; `cargo build` it before relying on it.
3. A transport implementing `HttpTransport` (sketch):
   - build an agent with `timeout_global = req.timeout`, `max_redirects(0)`, `http_status_as_error(false)`;
   - `post(url).header("Authorization", format!("Bearer {}", req.bearer.expose())).send_json(&req.body)`;
   - map timeout to `TransportError::Timeout`, other I/O to `Network`; read the body (cap 256 KiB), parse `Retry-After` seconds;
   - in-flight HTTP cannot be interrupted by `CancelToken`; the frontend bridge races abort and drops late data, and the backend re-checks the token after the call returns.
4. Tauri commands (register in `lib.rs` / `ipc.rs`, return `Result<_, ProviderError>`; `ProviderError` serializes as `{"kind","attempts"}`):
   - `ai_plan_intent(config: ProviderConfig, request: PlanRequest) -> Result<PlanOutcome-as-JSON, ProviderError>`; return the intent envelope JSON or `{"kind":"question","text"}`, which `RemoteIntentProvider` already parses. Keep a `HashMap<requestId, CancelToken>` and add `ai_cancel_plan(request_id)`.
   - `ai_test_connection(config: ProviderConfig) -> ConnectionReport`.
   - `PlanOutcome` needs a small `serde_json` conversion (envelope via `serde_json::to_value`); not added here to avoid an unused API.
   - The frontend `BackendIntentTransport` and `BackendConnectionTransport` are thin `invoke` wrappers; on rejection pass the serialized `{kind}` object through unchanged.
5. Persisting setup (endpoint/model/timeout) is a settings concern (integrator/UI): store only `ProviderSetup`; the API key is never part of it. Provisioning the credential in Credential Manager is a user action outside the app today; credential errors surface as `credential`.

## Test commands and results

See the task checkpoint comment for the exact output of the last run. Commands: `npx vitest run src/lib/ai`, `npm run check`, `npm run lint`, and from `contract-fixtures/ai/rust`: `cargo test`, `cargo clippy --all-targets -- -D warnings`.

## Known limits

- No live smoke test (no authenticated network request was made; user credential not touched).
- `SystemClock`, `OsCredentials` and `NullLog` are not unit tested (thin adapters; credential read is Windows-only).
- Cargo coverage was not measured (`cargo-llvm-cov` not used); the 21 provider tests exercise every public branch listed above.
- Non-Windows credential storage is unavailable (`CredentialError::Unavailable`).

## Onboarding provider in the app (FER-412 / AI-SEC-3)

End users no longer need the PowerShell script or `.env.local`. Setup has two non-overlapping parts:

| Part                                                   | Where it lives                                                                                                                                        | Code                                                                                                                                                                                                                                            |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Endpoint, model, timeout, structured mode (non-secret) | Backend file `ai-setup.json` in the app config dir (FER-430); browser storage `ferrum.ai.setup` only outside Tauri and as a one-time migration source | `src-tauri/src/ai/setup_store.rs` + `setup_commands.rs` (`ai_setup_load/save/clear`); frontend `src/lib/ai/providerSetupStore.ts` and `providerSettings.ts` (`initProviderSetup`, `saveProviderSetup`, `clearProviderSetup`, `loadStoredSetup`) |
| API key                                                | Windows Credential Manager, fixed target `Ferrum/AI/custom`, written/read only by Rust                                                                | `src-tauri/src/ai/credential.rs`, `credential_commands.rs`                                                                                                                                                                                      |

Backend commands (registered in `lib.rs`):

- `ai_credential_status() -> { configured }`: never returns the value.
- `ai_credential_save(value) -> { configured }`: validates (8-2048 bytes, printable ASCII, no whitespace), writes with `CredWriteW` (generic, local-machine persistence), reads back to verify, zeroes local copies. Errors are `{ kind }` with `invalid_value | unsupported | credential_failure`.
- `ai_credential_clear() -> { configured }`: `CredDeleteW`; absent credential is success (idempotent).
- Off Windows these report `unsupported` (status reports not configured). No Cargo change was needed: FFI is declared by hand like the existing reader.

Frontend rules: the key exists only in the password `<input>` until submit; the component empties the field before awaiting the backend, passes it only as the `value` argument of `invoke`, and never keeps it in `$state`, storage, or logs. After setup or key changes, `reloadAiRuntime()` (in `aiRuntime.ts`) drops the cached runtime so the next `aiRuntime()` call builds the provider from the new setup. The old runtime's 250 ms queue poll timer is not stopped (the runtime exposes no handle); it is harmless but a follow-up could stop it.

Dialog order (FER-429): Endpoint, Model, Timeout/Structured, API key, then one **Save** button (stores the setup, then the key if the field is filled; an invalid setup is rejected before the key is sent), a save state line (Not saved yet / Unsaved changes / All changes saved), then **Test connection** and its result. Test connection is enabled only when the setup and a key are stored and there are no unsaved edits; it always tests the stored setup (`loadStoredSetup`), never the form.

Test connection uses `testConnection` + `backendConnectionTransport` (fixed `ping`, no user text) and shows only the safe error kind.

Credential import scripts and `.env.local` remain valid developer paths.

### Setup storage in the backend (FER-430)

The non-secret setup used to live in webview localStorage, which is bound to the origin: a dev build (`http://localhost:5173`) and an exe build (`tauri.localhost`) each had their own copy while the key (Credential Manager) was shared. It now lives in `ai-setup.json` next to `config.json`, so every build on the machine sees the same endpoint and model.

- Not a field of `Prefs`: `Prefs` is rewritten whole on every UI change, so a frontend that does not send the AI setup would wipe it.
- `ai_setup_save` validates with `ProviderConfig::validate` (HTTPS or localhost+port, model charset, timeout 100..120000, fixed `credentialRef`) and writes atomically; `ai_setup_load` returns `null` for a missing, corrupt, hand-edited-unsafe or unknown-field file (a stray `apiKey` makes the file unloadable). Errors are safe kinds only: `unsafe_endpoint`, `invalid_config`, `storage_failure`.
- The frontend validates first, then calls the backend, and keeps a synchronous cache so `loadProviderSetup()` (used when the runtime is built) does not become async. `initProviderSetup()` runs at startup and when Settings > AI opens; if it changes the setup, the runtime is dropped and the open chat panel is rebuilt (history carried over).
- Migration: if the backend has no setup and a valid legacy browser copy exists, it is saved to the backend and the browser copy is removed only after the backend accepted it. When the backend has a setup, a stale browser copy is discarded. If the migration save fails the browser copy is kept and used for the session.
- The key is never part of any of this.

### Mounted in Settings > AI (FER-429)

The standalone `AiSettingsDialog` is gone. `AiSettingsSection.svelte` renders `AiSettings` inside `SettingsDialog` (one `AI` category entry plus one render line). Menu/palette action `view.aiSettings` and the chat "Open settings" banner call `aiChatHost.openSettings()`, which opens Settings and brings the AI section into view with focus on Endpoint.

### Test commands (FER-412)

`npx vitest run src/lib/ai/providerSettings.test.ts src/lib/components/ai/AiSettings.test.ts`; from `contract-fixtures/ai/rust`: `cargo test --test credential`, `cargo clippy --all-targets -- -D warnings`; from `src-tauri`: `cargo clippy --lib --tests`. Rust tests that touch the OS store only use the dummy target `Ferrum/AI/test-only-fixture-roundtrip`, never the user's `Ferrum/AI/custom`.

Not covered by automated tests: the real `ai_credential_*` commands against the user's credential target (by design), the Tauri IPC layer, and the visual layout inside SettingsDialog.
