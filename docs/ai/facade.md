# AI command facade (AI-02 / FER-159: FER-179..FER-182)

Status: code + unit/integration tests with the IPC boundary mocked. **No desktop E2E was run**, no real ZIP/sync touched
a filesystem from this lane. Nothing is mounted in `App.svelte` (integrator work, see Handoff).

The facade is the single bridge between AI intents and Ferrum's existing services. It builds no ZIP or sync engine.

## Files

| File                                                                                                                                | Role                                                                                                                                                  |
| ----------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/lib/packService.ts` (new, FER-180)                                                                                             | `defaultPackTarget`, `runPack` -> existing `archiveCreate` IPC. Used by the Pack dialog and the archive adapter.                                      |
| `src/lib/syncService.ts` (new, FER-180)                                                                                             | `compareForSync`, `buildSyncDryRun`, `runSyncPlan` (fresh compare, stale check, `enqueueCopy`, job ids). Used by `compareStore` and the sync adapter. |
| `src/lib/components/Dialogs.svelte`, `stores/dialogStore.svelte.ts`, `stores/compareStore.svelte.ts` (existing, minimal delegation) | Pure extraction: manual behaviour unchanged (characterization tests `DialogsPack.test.ts` and the existing `syncFlow.test.ts`).                       |
| `src/lib/ai/commands.ts` (FER-179)                                                                                                  | `CommandFacade`: `prepare` -> `approve` -> `execute`, `CommandReceipt`, typed `Result`/`CommandError`, `toExecutionReceipt`.                          |
| `src/lib/ai/adapters/archive.ts`, `adapters/sync.ts` (FER-179)                                                                      | Bridge: context lane resolution -> shared service.                                                                                                    |
| `src/lib/ai/commandsRegistry.ts` (FER-181)                                                                                          | One definition table: engine gate + mirrored UI actions + schema + adapter.                                                                           |
| `src/lib/ai/adapters/index.ts` (FER-181)                                                                                            | `ENGINE_GATES`, `createCapabilityRegistry`, `createCommandFacade` (default wiring).                                                                   |
| `src/lib/ai/commandsSelection.ts` (FER-182)                                                                                         | `SelectionHandleStore` (frozen manifest) and `summarizeJobs` (status from the queue).                                                                 |

## Lifecycle and trust

1. `facade.prepare(rawModelJson, { snapshot })` — read-only. `parseIntent` (AI-01) rejects unknown capability, extra fields
   (`approved`, shell, paths), wrong request id. The registry blocks inactive capabilities. The adapter resolves scope and target
   locally (context lane) and the facade builds a `ResolvedPlan` validated by `parseResolvedPlan`. Returns
   `{ handle: PreparedHandle, plan, preview }`. **Zero mutation**: no `archiveCreate`, no `enqueueCopy`, no selection writes.
2. `facade.approve(handleId)` — TRUSTED-ONLY (approval UI / policy, AI-05). Returns an `ApprovedPreparedCommand` that is
   registered in a private `WeakSet`; a model cannot produce one. Handles expire (default 10 min).
3. `facade.execute(approved)` — accepts only that object. Re-checks the capability gate, validates the handle against the plan
   (`assertPreparedForPlan`), then the adapter re-validates the snapshot and targets and calls the shared service. Idempotent per
   approval: concurrent or repeated calls return the same result and call the service once. A failure is cached, never auto-retried.
4. Receipt (`CommandReceipt`): `executionId`, `planId`, `jobIds`, `state: 'queued' | 'completed'`, `completed`, `conflictsSkipped`,
   `outputPath?`, `notes`. `toExecutionReceipt` maps it to the AI-01 wire receipt. Status is **never** derived from `runAction()`:
   that only says a handler was dispatched (e.g. a dialog opened). Tests prove `runAction('archive.pack')` calls no engine.

`queued` means the operation queue owns the work (sync: real `enqueueCopy` ids). `completed` means the service finished in-process
(archive: `archive_create` is awaited by the backend, so `jobIds` is empty). `summarizeJobs(receipt, lookup)` derives the live
outcome from the queue for exactly the receipt's job ids; a missing job is `unknown`, never success.

## Capability gates (FER-181)

| Capability         | Mirrors UI action      | Gate            | Why                                                                                                                                                                                                                                                                 |
| ------------------ | ---------------------- | --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `archive.create`   | `file.pack`            | ready           | Pack service works (F2-12).                                                                                                                                                                                                                                         |
| `directories.sync` | `commands.synchronize` | ready (FER-168) | AI-10 semantics (first-run union, persisted baseline, no newer-wins), verified baseline settlement (FER-414) and the backend approval guard for two-root scopes are in place; see `docs/ai/sync-chat.md`. Closing the gate is a one-entry revert in `ENGINE_GATES`. |

Availability = gate ready AND every mirrored action exists, has a handler and has no `disabledReason` (static; the manual
action's per-selection `available()` is not used because chat resolves its own scope). Pending => `adapterFor` returns
`blocked_capability` with the reason, for prepare and again for execute. `registry.describe()` is the model-facing manifest
(id, label, available, reason, JSON schema; no adapter). Schemas are checked against `parseIntent` in tests.

## Selection handle (FER-182)

`SelectionHandleStore.create(scope, entries)` copies and freezes the manifest paths. It does not reference pane selection, so
later selection changes, navigation, or the resolver releasing its manifest cannot alter an approved run, and running never writes
selection back to a pane. After approval a changed selection does not invalidate the run (spec 5); a changed tab, location,
filter or pane swap does (`stale`, nothing written). `toJSON` exposes only id, pane, count, digest.

## Known limitations and findings

- Pack is not a queue job: no job id, progress or cancel (`archive_create` is awaited); running it through the operation queue
  (job ids/cancel) is still open.
- **Destination conflict (FER-411, fixed).** The backend used to ignore `request.conflict` and `rename` over an existing file
  (which REPLACES it on Windows and Unix), so "overwrite" was the only behavior. `archive_create` now honors the policy
  (`archive_create_test.rs`): `skip` never replaces (an existing file or directory fails with `FsError alreadyExists`), `rename`
  takes the first free `name (n).ext` and returns the path actually written, `overwrite` stays explicit (the manual Pack
  dialog's default), `overwriteOlder` is rejected for creation. A name is claimed with an exclusive create, so there is no
  check-then-write window. The chat adapter uses `skip`; `outputExists` stays as a fast, clear first check, and the backend
  guard (FER-410) also refuses a name that appeared after the preview.
- The output file is created after the manifest is enumerated (`collect_files` runs before the `.ferrum-tmp` file), so a fresh
  output inside a recursed source does not become input. An older ZIP in the folder is included, as the spec allows.
- Default destinations differ by design: manual Pack = sibling `<folder>.zip`; chat = `<folder>\<folder>.zip` inside the source
  directory with unique-name suffix (context lane `resolveArchiveTarget`).
- `preview.totalBytes` is 0 (unknown): the manifest carries no sizes yet.
- Sync adapter refuses an incomplete compare (truncated/cancelled/errors), any plan with deletes, and plans with nothing executable. Conflicts are listed and skipped, never decided or executed. After `queued` it calls `settleSyncRun` (the only baseline writer).
- Digest binding is a local 128-bit FNV integrity check, not a cryptographic MAC. Backend verification of plan digest and
  idempotency keys is AI-06.
- `outputExists` must be wired by the integrator from the live pane listing (the context lane's `PaneProbe.allEntries()`).
- Tests mock the IPC boundary; ZIP content and queue behaviour on a real filesystem are AI-08/AI-11/AI-13 scope.

## Handoff for the integrator (not applied here)

```ts
// e.g. src/lib/ai/executor.ts (AI-06) or App wiring
import { createCommandFacade } from './adapters';
const { facade, registry } = createCommandFacade({
  archive: { flow, resolver, ctx: contextService, outputExists, onCompleted: refreshAllPanes },
  sync: { flow, ctx: contextService },
});
// provider prompt: registry.describe(); executor: facade.prepare -> (user approves) facade.approve -> facade.execute
```

No new Tauri command, Cargo or `package.json` change is required by this lane.

## archive.create: 7z and compression level (FER-576)

- `format` is `"zip"` (default) or `"7z"`. RAR is never creatable from chat (the intent parser rejects it, Rust and TS).
- `level` is optional, a closed set: `store | fast | normal | maximum | ultra | ultra-text`. Unknown strings, non-strings and `null` are invalid
  in both parsers (shared cases in `contract-fixtures/ai/cases.json`). The adapter maps it to the engine's `ArchiveCompressionLevel`
  (`maximum` and `ultra` both -> `max`, LZMA2 level 9 for 7z, deflate 9 for ZIP). The model never names an engine flag.
- `ultra-text` (FER-579) is a different method (7z PPMd, order 32 / 192 MiB), so it keeps its own name and is NOT folded into `maximum`
  (the one-name rule for `maximum`/`ultra` is unchanged). It is valid with `format: "7z"` only: both parsers reject it with zip, and the
  backend refuses it for ZIP/RAR. The prompt picks it when the user asks for the smallest/max 7z of plain-text files (csv, tsv, txt, log, json, xml, sql, md).
- Execution is the same Pack service as the manual dialog (`runPack`, `format: 'sevenz'`, conflict `skip`: never replaces a file).
- A typed `.zip`/`.7z` output name is converted to the chosen format's extension. Format and level are part of the plan digest, so
  changing either needs a fresh approval, and the preview text names them ("Format 7z, kompresi maksimum (level terkuat, sama dengan Max di dialog Pack)"); `ultra` is shown and digested as `maximum`.
- The provider prompt has a format rule ("paling kecil", "terkecil", "smallest", "max compression" -> `ultra`). The mutating-prompt
  golden in `provider_activity_tests.rs` was updated for this; re-run `docs/ai/verification.md` against a real provider.
