# AI executor, queue receipts and cancel (AI-06 / FER-163: FER-195..198)

Status: code + unit/integration tests with the IPC boundary mocked; Rust ledger tested through the fixture crate on a temporary
directory. **No desktop smoke, no real queue/filesystem run from this lane.** Nothing is mounted in the app (see Handoff).

## Files

| File                           | Role                                                                                                                                       |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `src/lib/ai/executor.ts`       | `AiExecutor implements ChatExecutor`: submit -> plan -> preview -> approval -> execute -> queue tracking, cancel, edit, recover/reconcile. |
| `src/lib/ai/executorState.ts`  | Run state machine (`canTransition`), `DependencyScheduler`, `summarizeRun` (status from queue job states).                                 |
| `src/lib/ai/executorLedger.ts` | `ExecutionLedgerPort` + `MemoryExecutionLedger` (same semantics as the Rust ledger; used by tests and Tauri-less runs).                    |
| `src/lib/ai/executorBridge.ts` | `backendExecutionLedger`: thin `invoke('ai_exec_*')` wrappers (commands NOT registered yet).                                               |
| `src/lib/ai/executorQueue.ts`  | `QueuePort` + `PollingQueuePort` (snapshot-based adapter, e.g. `() => ops.all`).                                                           |
| `src-tauri/src/ai/executor.rs` | `ExecutionLedger`: idempotency key, durable journal, reconcile, job ownership, `summarize`. Std only.                                      |
| tests                          | `executor*.test.ts`, `contract-fixtures/ai/rust/tests/executor.rs`                                                                         |

Policy/approval pieces are in `docs/ai/policy.md`.

## Flow (all state changes are the executor's)

```text
submit -> planning --provider--> resolving --facade.prepare (read-only)--> preview -> awaitingApproval
  approve (digest echo) -> queued -> ledger.reserve -> [backend guard] -> facade.execute (existing services)
  -> ledger.recordEnqueued(jobIds) -> running -> succeeded | partial | failed | cancelled
                                   \-> unknownOutcome (never replayed)
```

- The provider gets `capabilities` from the registry (`offeredCapabilityIds()` when present, else `describe()` filtered by `available`); nothing
  is hardcoded. A capability that is not available (a closed gate, e.g. `directories.sync` when reverted to PENDING) ends in `blockedCapability` with the registry reason and
  no prepare/compare/enqueue call.
- Zero mutation before approval: `prepare` is read-only; `execute` accepts only the facade token issued after the approval ticket check.
- Writes go only through the facade/adapters (Pack service, sync service). The executor has no ZIP/sync/copy logic.
- `succeeded|partial|failed|cancelled` come from `summarizeRun(jobIds, queue)` (job states + processed files + failures) or, for services that finish
  in-process (Pack), from the service receipt. `queued` is never success. A job missing from the queue after having been seen is `unknownOutcome`.
- Cancel during a partial copy: queued jobs are cancelled via the queue for **this execution's** job IDs only (ownership comes from the ledger).
  If files were already copied the end state is `partial` ("tanpa rollback") with the copied count in the receipt, not a plain `cancelled`.
- Pack (`archive_create`) is awaited by the backend, not a queue job: it has no job ID, so cancel reports honestly that it cannot be cancelled.
- Late responses: every async result is checked against the run epoch and final state. Cancel or edit aborts the provider signal, bumps the epoch,
  discards the prepared handle; a provider that ignores the abort signal and answers later changes nothing (tested).

## State machine

`planning -> resolving -> preview -> awaitingApproval -> queued -> running -> succeeded|partial|failed|cancelled`, plus `needsInput`,
`blockedCapability`, `stale`, `unknownOutcome`. Illegal jumps (e.g. `planning -> queued`, `awaitingApproval -> running`, any exit from a final state)
are rejected by `canTransition` and the executor does not emit them. `unknownOutcome` can only leave through explicit reconciliation (and never to `queued`).

## Dependency steps

> FER-576: multi-step requests (open a typed path, archive, delete) do not use `DependencyScheduler`: each step is its own run with its own card, see `docs/ai/chain.md`.

`DependencyScheduler` releases a step only after its `dependsOn` completed; a failed step skips its dependents; `cancel()` stops scheduling derived
steps (a running step may finish). Today the facade builds single-step plans and executes the whole plan on step 1; steps beyond the first are refused
as `unsupported_plan` (no fake execution) until an adapter produces real multi-step plans.

## Idempotency and crash boundary (backend, FER-196)

- Key = hash(execution id, plan digest), made by the executor. `reserve` writes the intent to a flushed journal **before** the write service is called.
- A second reserve for the same execution, or for the same plan with a new execution ID (provider retry, reconnect), returns the existing entry:
  no second job.
- Crash after enqueue but before the receipt is stored: on restart the entry is `UnknownOutcome` (journal replay turns un-confirmed `Reserved` into unknown).
  It owns no jobs and cannot be re-reserved. `resolve_unknown(Found(jobIds))` adopts jobs found in the queue; `resolve_unknown(NotFound)` frees the plan.
- A torn last journal line is ignored. Identifiers are validated (`[A-Za-z0-9_-]{1,128}`), so the tab-separated journal cannot be injected.

## Contract change in chat UI (reason)

`ChatExecutor.submit` now receives `runId` (the store creates it; the executor cannot otherwise route `RunUpdate`s to the right run). One line each in
`chatStore.svelte.ts` (interface + call). Existing tests are unchanged. Optional second argument `approve(runId, { planDigest })` lets the card echo the
plan digest it showed; a mismatch is `stale`. Without it the executor binds to the ticket it presented (UI is trusted code; the model has no path).

## Known limits / not tested

- `unknownOutcome` is final in `ChatStore` (`isTerminal`), so a later reconciliation result cannot update that card. The UI lane would need a "check result"
  action; the executor already exposes `recover()`, `reconcile(key, resolution)` and `summaryOf(executionId)`.
- The Rust ledger is not registered/wired; the TS ledger used in tests is in memory. The crash test simulates the process boundary with a persisted copy,
  not a real process kill. `PollingQueuePort` has no desktop test against `ops`.
- The backend `ApprovalLedger` guard (`ExecutionGuard`) is an injected port; its Tauri command and the path fingerprints are not wired.
- `listItems` needs an injected lister (`resolver.entries(handle)` for archive); without it only the total is returned.
- The conflict port is an interface; `Dialogs.svelte` wiring is UI/integrator work. Adapters skip (sync) or rename (archive) conflicts; no overwrite exists.
- No cancel for Pack; no rollback of files already copied.

## Handoff for the integrator (not applied)

1. `src-tauri/src/ai/mod.rs`: `pub mod approval; pub mod executor;`.
2. One managed `ExecutionLedger::open(<app data>/ai-exec.journal)` and `ApprovalLedger`; Tauri commands `ai_exec_reserve`, `ai_exec_record_enqueued`,
   `ai_exec_record_not_started`, `ai_exec_mark_unknown`, `ai_exec_resolve_unknown`, `ai_exec_by_execution`, `ai_exec_unresolved` (names used by
   `executorBridge.ts`; return shapes: `Reservation {kind:'fresh',key}|{kind:'existing',entry}`, `LedgerEntry {key,executionId,planId,planDigest,state,jobIds}`,
   camelCase serde). Optional: `ai_register_approval` / `ai_authorize_execute` for the guard.
3. App wiring (sketch):

```ts
const { facade, registry } = createCommandFacade({ archive: {...}, sync: {...} });
const queue = new PollingQueuePort({ read: () => ops.all, cancel: (id) => ops.cancel(id) });
queue.start(250);
const executor = new AiExecutor({
  provider, capture: (id) => ctx.capture(id), modelContext: (s) => ctx.modelContext(s),
  facade, capabilities: registry, ledger: backendExecutionLedger, queue,
  prefs: () => ({ autoCreate: prefs.aiAutoCreate }),   // default: assisted
});
const chat = new ChatStore(executor);
void executor.recover(); // surface unresolved executions after restart
```
