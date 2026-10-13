# Activity chat integration (ACT-08 / FER-235) - contract for a future runtime

Status (2026-10-05, update 2): option A was approved. **R1-R4 and R5-R7 v1 are implemented**
(IPC command, Rust intent, provider prompt section, TS contract/type split, executor read branch,
`answered` run state + answer card, "Open in panel"). The TS gate
`READ_CAPABILITY_GATES['activity.query']` is now `ready`; `activity.query` is offered to the model
only when the executor has an `ActivityQueryPort` (wired in `aiRuntime.ts`) **and** the activity
service is open (`activity.available`). Legacy callers (no port, `capabilities: None`) are unchanged
and `parseIntent` (TS) / `parse_intent` (Rust) still reject the read intent. Not yet done: the
end-to-end privacy audit, consent/disclosure copy, E2E with a real provider, v2 entity selector (see
"Implementation status R5-R7 v1" at the end). Tool code: `src-tauri/src/activity/chat_tool.rs`
(module `activity_chat_tool`), tests in `chat_tool_tests.rs`.

## Request mapping (FER-269)

`run_chat_tool(reader, &ChatRequest, &ChatContext)` maps to the same local queries as the UI:

| Request                  | Local query                                                                  |
| ------------------------ | ---------------------------------------------------------------------------- |
| `Recent{days,file}`      | `ActivityStore::recent` over `[now-days, now+1d)`                            |
| `Frequent{days,file}`    | `ActivityStore::frequent` from `now-days` to `now` (2 active UTC days rule)  |
| `FindByName{query,file}` | `search_candidates` + `activity_matcher` (same as `ActivityService::search`) |

- `days` is only 7, 30 or 90; anything else is `Rejected`, never rounded. `limit` default 10,
  clamped to 20, 0 is rejected.
- Name queries: path-like input (`\`, `/`, `X:`) is rejected so the tool cannot probe paths.
- Duplicate names: only the best-match tie (same match class and score) is considered. One entity
  gives `Resolved`; several give `Ambiguous{candidates}` with `parentHint` widened until pairwise
  distinct (falls back to appending the stable id). The tool never picks silently; the model must
  ask the user, then use the chosen `id`.

## Disclosure (FER-270)

Per entity only: `id` (stable), `displayName`, `parentHint` (parent path, tail-truncated to 48
chars with a leading ellipsis; widened only for disambiguation), `kind`, `availability`, `reasons`
(`recent`/`frequent`/`exactName`/`similarName`/`abbreviation`). Never: locator/full path, counts,
timestamps, scores, database contents, file content, favorites list, preferences.

Decision proposed here (decisions.md says only "no content or full DB is sent to AI" and does not
settle raw paths): **no raw path by default**. If product later wants full paths, it needs an
explicit decision plus per-request user consent; the field set is pinned by a test.

Suppression, all enforced in the tool in addition to the store:

- deleted/forgotten/excluded/cleared data is absent from the store, hence absent from output;
- `hidden_entity_ids` and `excluded_roots` (subtree boundary) from the caller are filtered, with
  over-fetch so hidden rows do not starve the limit; `limit` bounds output, `truncated` flags more;
- `private_session` returns `Withheld`; a stale privacy epoch returns `Stale` (retry, never reuse).

Caller duties: pass the real clock, current hidden ids, excluded roots and private-session flag.
`ActivityService::chat_context()` now returns an owned `ChatSnapshot` (clock, hidden ids from
`activity-settings.json`, `private_session = recording paused`, privacy epoch, taken under the store
lock); `snapshot.context()` borrows it as `ChatContext` with `expected_epoch = Some(epoch)`. Any
clear/forget/exclude/pause after the snapshot makes the tool answer `Stale` (never re-reads under a
newer epoch). `excluded_roots` in the snapshot is empty: the store has no read API for its exclusion
list, but it already deletes data under an excluded root and refuses to record it, so excluded items
cannot appear (tested). Ferrum has no separate "private session" concept; pause is the only flag, a
runtime may OR in its own.

## Real adapter (`ServiceReader`)

`ActivityService::chat_reader(workspace_root: Option<&str>)` returns `ServiceReader`, the
`ActivityReader` over the service store. It applies the UI filters: availability Missing/Offline/
Unknown dropped, optional workspace root (`*_in` store queries), retention floor, and over-fetch so
dropped rows do not starve the limit; hidden ids are applied by the tool from the snapshot. It calls
only store read methods (`recent_in`, `frequent_in`, `search_candidates_in`, `privacy_epoch`); a
test scans its source block (`chat-reader:begin/end` markers in `activity_service.rs`) for mutations.
Name matching in the tool uses popularity 0 (the UI's popularity boost is not applied; deliberate,
no ranking input from chat).

## Read-only and follow-up (FER-271)

- The tool reads only through the `ActivityReader` trait (4 read methods); no write path exists in
  its type surface. A test scans the module source and fails on any store mutation call, file
  system access or process spawn.
- `follow_up_reference(&ChatEntityRef) -> EntityReference{entityId, requiresPreparedPlan:true}`.
  ZIP/sync/copy/move/delete via chat must only carry that reference into the **phase-3
  prepared-plan + user approval flow** (`FOLLOW_UP_POLICY`). Execution must re-resolve the id
  through the local store/identity layer at plan time; the chat layer never holds a path.

## No ranking side effects (FER-272)

Chat lookups record no receipt, event, episode or access. Tests run 25 rounds of
recent/frequent/name queries, once on the bare store and once through the real service adapter
(after `flush()`), asserting `episode_count` and the frequent and recent results (counts, active
days, scores, timestamps) are identical before and after. A runtime must not report AI suggestions or user acceptance of them
as `ActivityReceipt`s; only a real, explicit Ferrum navigation/open does that.

## Wiring checklist (ACT-08 is NOT done)

Prepared and tested locally (no runtime needed): `chat_context()` snapshot, `ServiceReader`,
`Stale`/`Withheld` contract, filters, read-only guarantees.

Still open; items 1 and 4 need the AI contract change described in "Wiring proposal":

1. Register the tool with the provider using structured args matching `ChatRequestKind`; serialize
   `ChatResponse` (`status` tag, camelCase). Not done; no tool or IPC is registered.
2. In the runtime, take one `chat_context()` per tool call (or per turn) and keep using the same
   snapshot for the whole call; treat `Stale` as "retry with a new snapshot".
3. Decide the runtime's own private-session flag (OR it into `snapshot.private_session`) and the
   workspace root to pass to `chat_reader` (Ferrum has no persisted workspace entity).
4. Route follow-up actions (ZIP/sync/copy/move/delete) through phase-3 prepared plans + user
   approval only; re-resolve `entityId` locally at plan time.
5. Re-run the privacy audit end to end: no locator in provider payloads, tool results logged without
   path data, disclosure/consent copy for sending names to a provider.
6. Optional: store read API for the exclusion list (only needed to populate `excluded_roots`; the
   store already enforces exclusions).
7. IPC exposure: done (R1, `activity_chat_query`; R7 v1 `activity_chat_locate`). UI exposure: done
   (R5/R6 v1, see the end of this document).

## Seam analysis of the phase-3 runtime (2026-10-05, main 4a04a8d)

Read: `docs/ai/{facade,context,policy,provider,chat-ui,executor}.md`, `src/lib/ai/{aiRuntime,
commands,commandsRegistry,contracts,provider,executor,chatStore.svelte,backendBridge}.ts`,
`src-tauri/src/ai/{contract,provider,commands}.rs`. Findings:

1. **The provider protocol is single-shot, not tool-calling.** `ai_plan_intent` sends one chat
   completion (system prompt from `CAPABILITY_PROMPTS`, user text, `OutboundContext` with counts and
   flags only) and accepts exactly one JSON object back: `{"kind":"question","text"}` (<= 512 bytes)
   or an `IntentEnvelope` that `contract::parse_intent` accepts for an _offered_ capability. There is
   no second turn, no tool-result message, no free-text answer channel. A model therefore cannot
   "read the tool result and answer"; that needs a new request/response loop in `provider.rs`.
2. **The intent set is closed in both languages.** Rust `Intent` is a `serde(tag="capability",
deny_unknown_fields)` enum with two variants; `CAPABILITY_PROMPTS` is a fixed `[_; 2]` array and
   `is_known_capability`/`interpret` reject anything else. TS `ProposedIntent`/`parseIntent` mirror it
   and `CapabilityId = ProposedIntent['capability']` keys `INTENT_SCHEMAS`, `ENGINE_GATES` and
   `policy.BASE_RISK` (`Risk = 'mutating'` only).
3. **The facade/executor are mutation-only by construction.** `AiExecutor.plan` always goes
   provider intent -> `facade.prepare` -> `ResolvedPlan` (`risk:'mutating'`, `requiresApproval:true`)
   -> policy -> backend guard `register` -> `awaitingApproval`. `RunState`/`RunUpdate` have no
   "answer" outcome; the only model text shown is the question/`modelText` note.
4. **Mutation targets are panes, not entities.** `archive.create` selects via
   `sourcePane` + `selector` (`selected`/`allVisible`/`directoryContents`). There is no selector that
   can carry an `entityId`, so "zip folder itu" cannot reference a history result without a new
   selector (resolver + backend guard + contract).
5. `OutboundContext` is `deny_unknown_fields` and sent on every request; putting history into it
   ("context" seam) would send names to the provider for every chat message, unrequested. Rejected.

Conclusion: no read seam exists. Adding one means a new capability kind across `contract.rs`,
`provider.rs`, `contracts.ts`, the registry, the executor's run lifecycle and the chat UI, and (for
model-written answers) a new tool-result protocol. Per instruction this was **not** implemented;
nothing in the AI runtime, `ipc.rs` or `lib.rs` was changed.

## Wiring proposal (not implemented; needs owner decision)

### Option A (recommended): `activity.query` as a read intent answered locally

The model only _chooses the query_; the app runs it locally and renders the result card itself.
**No history result is ever sent to the provider** (stronger than "minimum metadata to the model"),
so no new consent copy for names is needed beyond the existing outbound disclosure of user text.

Wire intent (strict, `deny_unknown_fields` in both languages; field names as implemented in R2):

```json
{"capability":"activity.query",
 "query":{"kind":"recent","periodDays":7,"entity":"file"}
        | {"kind":"frequent","periodDays":30,"entity":"folder"}
        | {"kind":"findByName","name":"invoice 2026","entity":"folder"},
 "limit": 1..20 (optional, default 10)}
```

`periodDays` is exactly 7/30/90; `name` non-blank, <= 255 bytes, no `\` `/` `:` or control chars;
`limit` 1..20 (0, >20 and `null` are rejected, not clamped); no other fields. Maps 1:1 to
`ChatRequestKind`.

| Step | File(s)                                                                                                                                       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | Est.    |
| ---- | --------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------- |
| R1   | `src-tauri/src/activity_service.rs`, `src-tauri/src/ipc.rs`, `src-tauri/src/lib.rs`                                                           | `ActivityService::chat_query(&ChatRequest) -> ChatResponse` (one `chat_context()` + `chat_reader(None)` + `run_chat_tool`, one internal retry on `Stale`); IPC `activity_chat_query(request: ChatQueryWire)` with `Deserialize + deny_unknown_fields` wire struct -> `ChatRequest`. Rust tests: wire rejects extra fields/bad days/limit>20/path text; output keys pinned; `episode_count` and recent/frequent unchanged after N calls (FER-272 at IPC level); paused -> `withheld`. Independent of the AI contract; can land first.       | 0.5 d   |
| R2   | `src-tauri/src/ai/contract.rs`                                                                                                                | `Intent::ActivityQuery(ActivityQuery)` + validation in `parse_intent`. `Capability`/`ResolvedPlan` NOT extended (a read never produces a plan).                                                                                                                                                                                                                                                                                                                                                                                            | 0.25 d  |
| R3   | `src-tauri/src/ai/provider.rs`                                                                                                                | Third `CAPABILITY_PROMPTS` entry (array size 3) with the schema above and the rule "history questions only; never put paths; for a follow-up operation on a result ask the user to open it first". Provider tests: offered/not offered, rejection when not offered.                                                                                                                                                                                                                                                                        | 0.25 d  |
| R4   | `src/lib/ai/contracts.ts`, `src/lib/ai/commands.ts`, `src/lib/ai/commandsRegistry.ts`, `src/lib/ai/adapters/index.ts`, `src/lib/ai/policy.ts` | `parseIntent` branch; split `CapabilityId` into `MutatingCapabilityId` (existing Records keep their type) and `ReadCapabilityId = 'activity.query'`; a small read-capability entry (gate + schema, no `CommandAdapter`) so `offeredCapabilityIds()` can offer it; facade/policy untouched. Parity test vs Rust.                                                                                                                                                                                                                            | 0.5 d   |
| R5   | `src/lib/ai/executor.ts`, `src/lib/ai/activityQuery.ts` (new)                                                                                 | In `plan()`: intent `activity.query` -> `ActivityQueryPort.query()` (thin `invoke('activity_chat_query')`) -> `parseActivityAnswer` (exact key set `id, displayName, parentHint, kind, availability, reasons`; drops anything else) -> emit terminal state `answered`. Never calls `facade.prepare/approve/execute`, ledger, guard or queue. Vitest: key lock, no full path (parentHint <= 48 chars + leading ellipsis), `withheld`/`stale`/`ambiguous` mapped to explicit copy, port spy proves no `activity_record_visit` / facade call. | 0.75 d  |
| R6   | `src/lib/ai/chatStore.svelte.ts`, `src/lib/components/ai/PlanCard.svelte` (or new `ActivityAnswerCard.svelte`)                                | `RunState 'answered'` (terminal, label "Answered"), `RunUpdate.activity`; card lists names + parent hint + reason chips; ambiguous = "Which one?" list; withheld = "History is paused/private"; each row has "Open in panel" (an explicit user navigation, which may record a visit like any manual open).                                                                                                                                                                                                                                 | 0.5 d   |
| R7   | follow-up (FER-271)                                                                                                                           | v1: "zip folder itu" -> the card's "Open in panel" then the existing pane-based `archive.create` prepared plan + approval; zero new mutation path. `follow_up_reference()` `{entityId, requiresPreparedPlan:true}` is kept as the card's row id only. v2 (optional): selector `{kind:"entity", entityId}` resolved locally at prepare time + backend guard fingerprint.                                                                                                                                                                    | v2: 1 d |

Total option A without v2: about 2.5-3 days including tests and docs.

Risks: (1) changing the system prompt can shift model behaviour for `archive.create`/`directories.sync`
(re-run `docs/ai/verification.md` prompts against a real provider); (2) `CapabilityId` is used as a
`Record` key in several files, so the type split must be done carefully (compiler will flag misses);
(3) contract parity Rust/TS must be kept in lockstep (`commandsParity.test.ts`); (4) the user text
itself ("cari folder invoice 2026") still goes to the provider as today; that is already covered by
the existing outbound disclosure, but the copy should mention history questions.

### Option B: model-written answer from tool results (later, if wanted)

Adds a second provider turn: after R1-R5, send the parsed `ChatEntityRef`s (minimum metadata only) as a
tool-result message and accept a bounded text answer. Needs a new outbound type in `provider.rs`
(`deny_unknown_fields`, size cap, redaction), a new response kind, repair/retry rules for turn 2,
explicit per-request consent copy for sending names/parent hints to the provider, and the end-to-end
privacy audit. Estimate: +2-3 days. Not recommended before option A is used.

### Option C (rejected as primary): local keyword router before the provider

A `parseFollowUp`-style local parser could catch "file terakhir 7 hari" without calling the model. Cheap
and private, but it is regex NLU (Indonesian/English phrasing variants), and it still needs R1, R5 and R6.
Useful only as an offline fallback.

## Acceptance status per sub-task (2026-10-05)

| Task                            | Status                                                                                                                                                                                                                                                     |
| ------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| FER-269 request mapping         | Done at tool level, over IPC (R1) and in the app: the model is offered `activity.query` while the service is open and the executor runs it locally (R5).                                                                                                   |
| FER-270 disclosure              | Field set pinned at tool level, in the IPC JSON (R1) and again in TS (`parseActivityAnswer`, R5). No provider payload carries results (option A; tested at executor level).                                                                                |
| FER-271 read-only + follow-up   | Read-only proven at tool/adapter/IPC/executor level. Follow-up v1: "Open in panel" places the item in the active pane, then the existing pane-based prepared plan + approval (R7 v1). v2 entity selector built in FER-433 ("Buat ZIP" on the answer card). |
| FER-272 no ranking side effects | Proven at store, service-adapter and IPC level (R1) and at TS level: a query only invokes `activity_chat_query` (never `activity_record_visit`), locate only `activity_chat_locate` (Rust test: episode count unchanged).                                  |

Still not done regardless of option: end-to-end privacy audit, consent/disclosure copy update, E2E chat
with a real provider (no credentials in this lane).

## Implementation status R1-R4 (2026-10-05)

Rule kept throughout: the model only _chooses_ a strictly typed query; the app runs it locally and
history results are never sent to the provider. No change to the executor, approval, mutation policy
or the semantics of `archive.create` / `directories.sync`.

| Step           | Status | Where                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   | Tests                                                                                                                                                                                                                                                                                                                                                                                                       |
| -------------- | ------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R1 IPC         | Done   | `ActivityService::chat_query` (fresh `chat_context()` + `chat_reader(None)` + `run_chat_tool`, one retry on `Stale`); `ipc::activity_chat_query(request: ai::contract::ActivityQuery)` + testable `activity_chat_query_on`; registered in `lib.rs`. Invalid values -> `FsError{code:invalidPath,"invalid activity query"}`; store missing -> `{"status":"unavailable"}`.                                                                                                                | `src-tauri/src/ipc_activity_chat_tests.rs`: strict wire (unknown fields, kind/period/name/limit shapes), semantic rejects incl. path-like names, default limit 10 / max 20, file vs folder, paused -> `withheld`, 25x5 queries leave `episode_count`, recent/frequent items (counts, days, scores, timestamps) and epoch unchanged, JSON key set pinned and no locator / count / score / timestamp strings. |
| R2 Rust intent | Done   | `contract.rs`: `Intent::ActivityQuery(ActivityQuery)`, `HistoryQuery`, `HistoryEntity`, `validate_activity_query`, `Intent::capability_id`, `parse_intent_offered(value, id, offered)`. `parse_intent` (legacy) still rejects `activity.query`; old variants unchanged. `Capability`/`ResolvedPlan` not extended.                                                                                                                                                                       | `src-tauri/src/ai/contract_activity_tests.rs` + shared fixture `contract-fixtures/ai/activity-query-cases.json` (26 cases).                                                                                                                                                                                                                                                                                 |
| R3 provider    | Done   | `provider.rs`: separate `READ_CAPABILITY_PROMPTS: [_; 1]` (the mutating `CAPABILITY_PROMPTS: [_; 2]` is untouched); prompt section appended after the mutating ones, before the unchanged tail. `capabilities: None` (legacy) = mutating only. `interpret` uses `parse_intent_offered`. `gate_read_capabilities` drops `activity.query` when the activity store is unavailable; called in `ai_plan_intent`.                                                                             | `src-tauri/src/ai/provider_activity_tests.rs`: golden copies of the old head/archive/sync/tail text, byte-identical prompt for every old offer combination, new text only appended, accepted only when offered, availability gate.                                                                                                                                                                          |
| R4 TS          | Done   | `contracts.ts`: `MutatingCapabilityId`, `ReadCapabilityId`, `ActivityQuery*` types, `parseOfferedIntent`, `parseActivityQuery` (`parseIntent` unchanged). `commands.ts`: `CapabilityId = MutatingCapabilityId \| ReadCapabilityId`; `INTENT_SCHEMAS`, `ENGINE_GATES`, `BASE_RISK`, adapters and registry entries keyed by `MutatingCapabilityId` (type-only change). New `readCapabilities.ts`: `READ_CAPABILITY_GATES` (pending), `offeredReadCapabilityIds`, `ACTIVITY_QUERY_SCHEMA`. | `src/lib/ai/activityQueryContract.test.ts` (same shared cases, bound parity with the Rust constants). No existing test changed.                                                                                                                                                                                                                                                                             |

Offer decision (R3): `activity.query` is offered iff (a) the TS gate is ready (after R5-R7) and (b) the
activity service is available (store opened). Paused recording does **not** withdraw it: the local
query answers `withheld` ("history is paused"), which is more honest than the model not knowing the
capability. The backend re-applies (b) in `ai_plan_intent`.

IPC for R5 (thin port): `invoke('activity_chat_query', { request: { query, limit? } })`, where
`query`/`limit` are copied from the parsed intent (validate with `parseActivityQuery`). Response:
`ChatResponse` (`status`: `entities | resolved | ambiguous | notFound | withheld | stale | rejected |
unavailable`).

Contract note for docs/ai (not edited there to avoid conflicts with FER-170): the provider output rule
in `docs/ai/provider.md` ("an `IntentEnvelope` accepted by `contract::parse_intent`") now reads
"accepted by `contract::parse_intent_offered` for an offered capability"; the effective set for
existing callers is unchanged.

Remaining after R1-R4: see "Implementation status R5-R7 v1" below.

## Implementation status R5-R7 v1 (2026-10-05)

Same rule: the model only chooses a strictly typed query; the app answers it locally; the answer is
never sent to the provider (no second turn, nothing in `OutboundContext` or the model context). The
mutation path (prepare -> policy -> guard -> approval -> ledger -> queue) and its tests are unchanged.

| Step                    | Status         | Where                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Tests                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| ----------------------- | -------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| R5 executor read branch | Done           | `src/lib/ai/activityQuery.ts` (new): `ActivityQueryPort` (`available`, `query`, `locate`); `parseActivityAnswer` (exact key set per `status`; item keys `id, displayName, parentHint, kind, availability, reasons`; any extra field such as a locator, counts, scores or timestamps rejects the whole answer; parent hint <= 49 chars for listed/resolved items, <= 400 for widened ambiguous candidates; at most 20 items; names without `\` or `/`); `tauriActivityQueryPort` (thin `invoke('activity_chat_query', { request })`; this window's private session answers `withheld` without invoking). `provider.ts`: `ProviderEvent` gains `{kind:'activityQuery', request}`; `parseModelReply` uses `parseOfferedIntent` only for a reply naming an offered `activity.query`, every other reply still goes through `parseIntent` (remote and fake providers). `executor.ts`: `ExecutorDeps.activity?`; `offered()` appends `offeredReadCapabilityIds` only when a port exists; an `activityQuery` event goes to `answer()`: re-validate with `parseActivityQuery`, re-check the offer, `port.query`, `parseActivityAnswer`, terminal state `answered`. Never calls facade/policy/guard/ledger/queue. IPC errors: `invalidPath` -> `rejected`, others -> `unavailable`; malformed data -> `failed` (nothing shown). An `activityQuery` reply during a plan revision -> `needsInput` (plan unchanged). `readCapabilities.ts`: gate `ready`. `aiRuntime.ts`: wires the Tauri port with `activity.available` / `activity.privateSession`. | `src/lib/ai/activityQuery.test.ts` (strict parser, copy, IPC port: only `activity_chat_query` / `activity_chat_locate`, private session, offered-only parsing); `src/lib/ai/executorActivity.test.ts` (offer on/off/legacy list, answered without facade/guard/ledger/queue, one provider request and no names/hints/ids in any provider payload or model context, strict rejection, status mapping, service gone, cancel drops the late answer, mutating intents still planned, real Tauri port only invokes `activity_chat_query`). `activityQueryContract.test.ts`: gate assertion updated (pending -> ready). |
| R6 state + card         | Done           | `executorState.ts`: `planning -> answered` (terminal). `chatStore.svelte.ts`: `RunState 'answered'` (label "Answered from local history"), `RunUpdate.activity/activityQuery` (re-validated with `parseActivityAnswer` in the store; invalid -> `failed`), `RunView.activity/activityQuery/chosenId/opening/openNote`, `chooseCandidate`, `openActivityItem`, `ChatExecutor.locateActivityItem?`. `components/ai/ActivityAnswerCard.svelte` (new), rendered by `ChatPanel` for `answered` runs (other runs keep `PlanCard`): items with name, parent hint, kind, availability and reason chips; Indonesian messages for withheld/stale/rejected/unavailable/notFound (role `alert` for warnings/errors; the backend reason text is never shown); ambiguous = candidate list with parent hints and "Pilih" buttons, the choice is local UI state only and moves focus to "Buka di panel"; `role="status"` live region, labelled list and buttons, keyboard via native buttons (the panel still stops key propagation); privacy line "hasilnya tidak dikirim ke provider AI". No Run/Edit/Cancel on an answer.                                                                                                                                                                                                                                                                                                                                                                                                                             | `src/lib/components/ai/ActivityAnswerCard.test.ts` (store: terminal, re-validation, choose, open; card per status, ambiguous pick + focus, missing item without button, no locating executor -> no button, refused location note, keyboard isolation).                                                                                                                                                                                                                                                                                                                                                            |
| R7 v1 follow-up         | Done (minimal) | "Buka di panel": `ChatStore.openActivityItem` -> `AiExecutor.locateActivityItem` (only ids listed in that run's answer, not missing/offline, kind must match) -> port `locate` = new read-only IPC `activity_chat_locate(entityId)` (`ActivityService::chat_locate`: identity lookup only; `null` when paused, hidden, unknown, detached or Missing/Offline/Unknown; returns `{path, parentPath, kind}`) -> `revealInPane` (folder: `openPath`; file: `revealPath`, else `openPath(parent)`) on the active pane. The path stays in the renderer. No mutation and no direct ZIP/sync path: the user selects the scope in the pane and asks for a new plan, which goes through the normal prepared plan + approval. The pane navigation is an ordinary explicit navigation (it may record a visit like any manual open; the query never does).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             | Rust `ipc_activity_chat_tests::locate_gives_the_local_path_of_a_listed_entity_only_and_records_nothing` (path and key set, unknown/blank/oversized ids, hidden, paused, missing store, episode count and epoch unchanged); TS executor/card tests above.                                                                                                                                                                                                                                                                                                                                                          |

Still open (not part of v1):

- **End-to-end privacy audit** (provider payload capture against a real build, logs, crash reports).
- **Consent/disclosure copy**: the outbound disclosure should say that history questions are sent as
  text (the answer itself is not sent); the card already states this locally.
- **E2E with a real provider**: re-run `docs/ai/verification.md` prompts with `activity.query`
  offered (the system prompt grew by one section; model behaviour for `archive.create` /
  `directories.sync` must be re-checked). No credentials in this lane; not run.
- ~~v2 entity selector~~ Built in FER-433, see "R7 v2 (FER-433)" below.
- Workspace root for `chat_reader` and an OR of the runtime private-session flag on the Rust side
  (today the TS port answers `withheld` for this window's private session before calling the IPC).
- Desktop/GUI smoke of the card (jsdom tests only).

## R7 v2 (FER-433): entity selector, "Buat ZIP" on the answer card

Scope as built (the task text was empty; this follows the "v2 (optional)" line of R7):

- Selector `{kind:"entity", entityId}` on `archive.create` (`contracts.ts` `EntitySelector`). It is **app-only**: the
  card button builds the envelope (`entitySelector.ts` `entityArchiveEnvelope`); the model never sees an `entityId`
  (there is still no second provider turn) and never proposes the selector. `parseIntent` rejects it unless the
  executor passes `allowEntitySelector`; Rust `Intent`, `CAPABILITY_PROMPTS`, `INTENT_SCHEMAS` and the provider
  prompt are unchanged. The previous-plan summary sent to a provider on a follow-up redacts the id.
- Flow: card button -> `ChatStore.zipActivityItem` (only an item listed in that answer, not missing/offline, not
  while another plan is active) -> `AiExecutor.submitEntityArchive` -> normal `buildPlan` -> `facade.prepare`
  -> archive adapter resolves the entity **at prepare time** through the same read-only `activity_chat_locate`
  port as "Buka di panel" (`null` when paused, hidden, private session, unknown, detached, Missing/Offline; the
  plan then stops at `needsInput` and nothing is registered) -> `ScopeResolver.resolveEntity` builds a frozen
  one-item manifest (paths stay in the renderer).
- Target is always the **active pane's folder** (`target: {kind:"pane", pane:"activeAtSubmit"}`): the history item is
  not necessarily visible in any pane, so `sourceDirectory` has no meaning. A non-folder view (search, branch,
  archive) is refused. Default name: folder name, or file name without the last extension; reserved/invalid
  names fall back to the target default.
- Everything after prepare is unchanged: preview, user approval, `ai_register_approval` fingerprint of the item path
  (Rust re-checks at `ai_authorize_execute`), ledger, queue. No new mutation path and no Rust change.
- Local follow-ups (rename, left/right target, default name) still work and re-resolve the entity; extension
  filter and "hanya file" are refused for this selector.
- Privacy rules kept: recording-off/pause, hidden items, private session and the disclosure gate are enforced by
  the existing port/IPC (`available()` and `locate`), unchanged. The click itself records nothing.
- Tests: `src/lib/ai/entitySelectorFlow.test.ts` (real executor + facade + adapter), card tests in
  `src/lib/components/ai/ActivityAnswerCard.test.ts`.
- Not done: Playwright/desktop smoke of the new button (jsdom + real-stack unit tests only); selecting several
  history items at once; letting the model reference history results (needs a second provider turn).
