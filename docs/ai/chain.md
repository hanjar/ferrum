# AI chain: open a typed path, archive, delete to the Recycle Bin (FER-576 B/C)

Owner request that drove this: "`d:\Downloads\...\x.csv` -> arsipkan ke paling kecil gunakan 7zip lalu hapus".

The chat gets three things. Each is small and fail-closed; the chain only strings them together.

| Step              | Capability                           | Plan + approval card?     | Writes anything?          |
| ----------------- | ------------------------------------ | ------------------------- | ------------------------- |
| Open a typed path | `pane.navigate`                      | no (read-only, automatic) | no (only changes a panel) |
| Archive           | `archive.create` (+7z/level, part A) | yes                       | creates the archive       |
| Delete            | `items.delete`                       | yes, a **warning card**   | Recycle Bin only          |

## `pane.navigate`

`{"capability":"pane.navigate","pane":"left"|"right"|"activeAtSubmit","path":"D:\\x\\a.csv"}`

- The model only **copies** a path the user typed. It is accepted only if the path occurs in the user's own request text
  (case-insensitive, `/` = `\`, quotes ignored, matched at a token boundary; a dot inside a name is not a boundary). This is
  checked in three independent places: the Rust provider (`interpret_for`), the contract parsers (`parseChain`, Rust and TS)
  and the executor (`runNavigate`). Shared cases: `contract-fixtures/ai/navigate-cases.json`.
- Contract: absolute, a local drive letter, no `..`, no device names, no stream (`:`) syntax, no UNC, at most 1024 bytes.
- Disk: the path must exist (`path_resolve`, the address bar's own gate). A path that the resolver changed (for example
  `%VAR%` expansion) counts as missing. A file opens its folder and selects the file. An archive file is never entered.
- Nothing is executed or opened with a program. No listing, name or size goes to the provider.

## `items.delete`

`{"capability":"items.delete","sourcePane":..., "selector":{"kind":"selected"}}`

- The target is only the panel's marked selection (no cursor fallback: nothing marked = a question). No field can name an item,
  a path or a name, and there is **no** `permanent` field anywhere (wire, adapter, IPC wrapper).
- Backend: `ai_delete_to_recycle` has no `permanent` argument and never calls `delete_permanent`. It applies the same source
  gate as the manual Delete (path gate, protected folders, local-UNC), and refuses the **whole batch** before touching anything
  if any item cannot go to the Recycle Bin: not a fixed local drive-letter volume (USB, network, optical), UNC, or a reserved
  Windows name. `ai_recycle_check` reports the same thing when the card is built. No fallback to a permanent delete.
- The card is a warning card: red box with the count and the item names (read locally), the text that items go to the Recycle
  Bin, a distinct "Pindahkan ke Recycle Bin" button that is not focused by default (Cancel comes first), no Edit button.
  Policy classes it `mutating`, so even "auto-create" can never approve it.
- The backend approval ledger fingerprints the items when the card is built (root = the panel folder) and re-checks them
  right before the delete runs. A follow-up ("hanya csv") on a delete is refused.
- Partial result (some items could not be moved) is `partial` with the failed names; nothing is retried.

## Chain (option B: sequential, one card per step)

The provider may answer with `{"schemaVersion":1,"requestId":...,"steps":[intent,...]}` (2 to 4 steps) instead of an envelope.
The grammar is closed (Rust `parse_chain_offered`, TS `parseChain`, shared cases `contract-fixtures/ai/chain-cases.json`):
`pane.navigate`? then `archive.create`? then `items.delete`?, each at most once, one panel for the whole chain, every step
offered and valid as a single intent, and an archive that feeds a delete must archive the panel selection.

Execution (executor):

1. Step 1 runs in the run the user started. `activeAtSubmit` is bound to a concrete side now, so clicking the other panel
   cannot redirect a later step.
2. Each later step is its **own run** with its own card, planned against the panels as they are when that step starts (after
   the panels finished re-listing). The remaining steps live on the run that is executing a step and are dropped by any
   failure, refusal, stale result or cancel. Cancelling any card stops the rest.
3. Every step registers its paths with the backend guard when **its** card is built (not at the start of the chain) and is
   re-validated by the backend right before it runs.
4. "Archive, then delete": after the archive was written the executor runs **Test Archive** (`archive_test`; every entry must
   pass). Only then is the delete run created. If the test fails, cannot run, or no verifier is wired, the archive run ends
   `failed` ("File asli TIDAK dihapus") and no delete card exists.
5. The delete step must see the **same selection digest** as the archive step; if the selection changed in between it ends
   `stale` and deletes nothing.
6. The provider is asked once. Steps 2 and 3 are resolved locally; names never go to the provider.

Where it lives: `src/lib/ai/executor.ts` (`startChain`, `runChainStep`, `makeChainRun`, `chainGate`, `verifyChainArchive`),
`adapters/delete.ts`, `paneNavigate.ts`, `navigatePort.ts`, `PlanCard.svelte`, `src-tauri/src/ai/contract.rs`,
`ipc.rs` (`ai_recycle_check`, `ai_delete_to_recycle`), `mutations.rs` (`recycle_blocker`).
Tests: `executorChain.test.ts` (real executor/facade/adapters), `chainContract.test.ts`, `navigatePort.test.ts`,
`PlanCardDelete.test.ts`, `contract_chain_tests.rs`, `provider_chain_tests.rs`, `mutations.rs::recycle_tests`.

## Not covered / limits

- `settled()` is a best-effort wait for panels to finish re-listing; a snapshot taken too early is refused as stale, never acted on.
- The prompt is not yet verified against a real provider (`docs/ai/verification.md`): in particular that the model copies the path
  exactly and chooses the chain for the owner's sentence.
- Windows only (the Recycle Bin check uses `GetDriveTypeW`).
