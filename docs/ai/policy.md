# AI policy, preview and approval (AI-05 / FER-162: FER-191..194)

Status: code + unit tests (IPC mocked; Rust tested through the fixture crate on a temporary directory). No desktop smoke.
Nothing is mounted in the app (integrator work, see Handoff).

## Files

| File                                     | Role                                                                                                                    |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `src/lib/ai/policy.ts` (FER-191)         | `evaluatePolicy`, `classifyRisk`, `DEFAULT_PREFS`, `isIssuedDecision`.                                                  |
| `src/lib/ai/approval.ts` (FER-192/193)   | `ApprovalManager` (ticket + binding digest), `decideConflicts`, `ConflictPort`.                                         |
| `src-tauri/src/ai/approval.rs` (FER-194) | `ApprovalLedger`: backend revalidation + `StaleReport`. Std only. Tests: `contract-fixtures/ai/rust/tests/approval.rs`. |

## Policy (FER-191)

Everything is computed locally from the trusted `ResolvedPlan` (facade output) and the user preference. Model fields
(`approved`, `risk`, `requiresApproval`) are never read: `parseIntent` already rejects extra fields, and the policy ignores them
even if a plan object carries them.

| Case                                                     | Decision                                                  |
| -------------------------------------------------------- | --------------------------------------------------------- |
| capability not available (gate pending, action disabled) | `blocked` with the registry reason; never approved        |
| default (`autoCreate: false`, "assisted")                | `needsApproval`                                           |
| `autoCreate: true`, `archive.create`, no conflicts       | `autoApproved` (new output, no overwrite)                 |
| `directories.sync`                                       | always `needsApproval` (approval of one specific dry-run) |
| any plan with conflicts                                  | risk `mutating`, `needsApproval`                          |

A `PolicyDecision` is registered in a private `WeakSet`; `ApprovalManager.autoApprove` accepts only an issued
`autoApproved` decision, so a hand-built object cannot trigger execution.

## Approval binding (FER-192)

`ApprovalManager.present(runId, prepared, {conflictDecision})` builds an `ApprovalTicket` from the facade's `PreparedCommand`:
plan id, plan digest, revision (`plan.revision` + number of edits), root (target directory or `left|right`), item scope (manifest
ref), item count, conflict decision and expiry. `bindingDigest` hashes all of them. The UI must hand back the digest it showed:

- no ticket for the run -> `not_approved` (fabricated approval);
- digest differs (plan changed, other run, other decision) -> `stale`;
- past expiry -> `expired`;
- otherwise `facade.approve(handleId)` issues the only token `facade.execute` accepts.

`approve` does not call the adapter: approval alone mutates nothing (zero files/jobs before approval).

## Conflicts and edits (FER-193)

- One approval per ordinary batch: repeated `approve` returns the same token (`approvalCount === 1`).
- The existing conflict dialog is reached through the `ConflictPort` (`decide(conflicts) -> 'skip' | 'rename' | 'cancel'`), asked once
  per batch by `decideConflicts`. There is deliberately **no `overwrite`** decision and `present` throws on it; a closed or failing dialog
  is `cancel`. The decision is part of the binding digest. The existing adapters skip conflicts (sync) or rename (archive), so
  nothing is overwritten from chat.
- A plan edit calls `invalidate(runId)`: the ticket and token are dropped and the epoch grows; the next `present` has a different
  digest and revision, so the old approval cannot approve the new plan.

## Backend revalidation (FER-194)

`ApprovalLedger` (Rust) is registered with the approved facts (`ApprovalRecord`: handle, plan id, digest, revision, root, source
fingerprints = path + kind + size + mtime, output name, expiry). `authorize(ExecuteClaim)` re-checks at execution time and returns
`Err(StaleReport { reasons })` with zero writes:

`UnknownApproval`, `AlreadyConsumed` (single use, double submit), `Expired`, `PlanMismatch`, `DigestMismatch`, `RevisionMismatch`,
`RootMismatch`, `ScopeMismatch` (item-scope digest), `SourceMissing`, `SourceChanged`, `TargetMissing`, `TargetNotWritable`,
`OutputExists` (old target stays intact).

## Not covered / limits

- Writability is the read-only attribute only (a probe write would be a mutation); the engine still enforces real permissions and
  disk-full at write time. Symlink/alias root identity is left to the engine (`resolveSync` only compares strings).
- Source fingerprints are per item (top-level paths); changes deep inside a selected folder are not detected here. The sync adapter
  re-runs its own compare (`runSyncPlan` stale check) and Pack enumerates at run time.
- The digests are integrity bindings (FNV), not cryptographic MACs.
- The conflict port is an interface; wiring it to `Dialogs.svelte` is integrator/UI work.

## Handoff (integrator, not applied)

1. `src-tauri/src/ai/mod.rs`: add `pub mod approval;` (and `pub mod executor;`, see `docs/ai/executor.md`).
2. Tauri commands (in `ipc.rs`/`lib.rs`) wrapping one shared `ApprovalLedger` in managed state: `ai_register_approval(record)` after the user approves and
   `ai_authorize_execute(claim)` immediately before the frontend calls the write service. Not done here: those files are not owned by this lane.

## Backend approval guard (FER-410 / AI-SEC-1) — wired

The Rust `ApprovalLedger` is now connected through three Tauri commands (`src-tauri/src/ai/approval_commands.rs`) and the
TypeScript `createBackendGuard` (`src/lib/ai/backendGuard.ts`), installed in `src/lib/ai/aiRuntime.ts`.

| Step                                      | What happens                                                                                                                                                                                                                                                                      |
| ----------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| preview is built                          | `register`: the **backend** fingerprints every source (size + mtime, directories by mtime), stores the approval slot (plan id/digest, revision, root, output name, expiry) and returns an opaque `scopeDigest`. A failure here fails closed: no preview, nothing can be approved. |
| user approves, executor is about to write | `authorize`: backend clock (never the frontend's), digest/root/scope match, sources unchanged, target exists and is not read-only, output name still free. Single use.                                                                                                            |
| any mismatch                              | rejection `{kind:"stale", reasons:[codes], summary}` (codes only, no raw path) → run state `stale`, zero writes.                                                                                                                                                                  |
| edit / cancel / failure                   | `revoke`: the slot is dropped.                                                                                                                                                                                                                                                    |

Fail-closed rules: a capability without a backend guard cannot register, so it cannot be approved; an unreadable source, a malformed id
or path, or an IPC error never produces an approval. `directories.sync` now has its guard (FER-168, `docs/ai/sync-chat.md`): the same
register/authorize pair carries a second root (`secondRoot`), the fingerprinted paths (sources, overwrite targets, containing directories, both
roots) and the targets of new copies that must stay absent (`absentPaths`); the scope digest binds all of it, both roots are compared in
canonical form (a junction redirect is `root_mismatch`), and a new rejection code `target_appeared` exists. ZIP requests are unchanged.

Known limits (honest): the fingerprint of a **directory** is its own mtime, so a change deeper inside a selected folder is not
detected; writability is only the read-only attribute (a real probe would mutate the disk), the engine still enforces permissions
when writing; up to 250,000 sources per approval. Verified by `scripts/ai-desktop-smoke.mjs` (a source changed after the preview
makes the run stale, with the backend's own wording, and nothing is written).

## activity.query (read-only)

`activity.query` (ACT-08) is a read capability, separate from the mutating registry above:

- The model only chooses a strictly typed query; the app runs it locally (`activity_chat_query`) and renders the answer itself. History results are **never** sent to the provider and there is **no second provider turn**.
- It does not go through preview, approval or the execution ledger: it reads and records nothing (no receipt, episode or ranking input).
- Any follow-up that changes files goes only through the normal prepared plan + approval described above; "Open in panel" only places the item in a pane.
- Paused recording or this window's private session answers `withheld` (no history shown).
- It is offered only when the ActivityService is available **and** the first-run activity disclosure has been acknowledged (FER-432; frontend port `available()` and backend `gate_read_capabilities` via `activity_service::chat_offer_ready`).
- The query line on the answer card is labelled as the model's request and the model-chosen name is quoted and cut to 60 characters.
