# AI-03 — Two-pane context and scope resolver

Task: AI-03 / FER-160 (FER-183 snapshot, FER-184 resolver, FER-185 target, FER-186 tests). Branch `feature/ai-b-context`, base `9a33c2229d6bc160762256165d07c4bee355e482`. All code is new and read-only toward existing stores (`src/lib/ai/context*`); no existing file was modified.

## Modules

| File                 | Role                                                                                                                                                                                                                                                                                                                                                                                 |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `context.ts`         | `ContextService`: `capture(requestId)` -> frozen `PaneSnapshot` (AI-01 contract), stable tab IDs (`tabIdOf`, WeakMap on the tab object, never the tab index), context/selection revision counters, `check()` staleness (`tab_gone`, `pane_swapped`, `context_changed`, `selection_changed`), `selectionPaths()` (frozen local copy), `modelContext()` (counts/flags only, no paths). |
| `contextResolver.ts` | `ScopeResolver.resolve(snapshot, paneRef, selector)` -> `ScopeOk` (counts, exclusions, Indonesian `scopeText`, opaque `handle`) or typed error. Frozen item list via `entries(handle)` is local-only. `revalidate()` before execution.                                                                                                                                               |
| `contextTarget.ts`   | `resolveArchiveTarget`: output directory from the snapshot pane, default name `<folder>.zip` (`archive.zip` for roots/reserved names), `sanitizeOutputName`, case-insensitive collision -> `Name (2).zip`, output-inside-source detection, `excludeFromInput`.                                                                                                                       |
| `contextFlow.ts`     | `ContextFlow.resolveArchive/resolveSync/revalidate`: IntentEnvelope + snapshot -> scope+target; rejects request-ID mismatch, cancelled requests, stale context; sync roots must be distinct, non-nested, plain directory views.                                                                                                                                                      |
| `contextPanes.ts`    | `buildPaneProbe`, `registerPaneContext`, `registryHost`: read-only bridge from a real pane to `ContextHost`.                                                                                                                                                                                                                                                                         |
| `contextTestKit.ts`  | In-memory fake host for tests.                                                                                                                                                                                                                                                                                                                                                       |

## Scope table (as implemented)

| Selector            | Source                                                                                        | Notes                                                                                                                                                                       |
| ------------------- | --------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `selected`          | selection frozen at submit (`capture`)                                                        | Empty -> `empty_selection` ("Belum ada item yang dipilih ..."); never select-all. A selected path missing from the listing -> `stale_context`.                              |
| `allVisible`        | `probe.visibleEntries()` = full post sort/filter/hidden list, NOT the rendered virtual window | `filesOnly` keeps regular files only (folders and symlinks skipped, counted in `excluded.nonFiles`). Hidden and filter exclusions are counted and written into `scopeText`. |
| `directoryContents` | `probe.allEntries()` (whole loaded listing), ignores the pane filter                          | `includeHidden` honoured. This is the pane's loaded listing, not a fresh backend enumeration; recursion into subfolders is Pack's job.                                      |

Unsupported source views (`branch`, `search`, `archive`) return `unsupported_view` with an explicit message; nothing falls back silently.

## Staleness rules

- Context key per pane: tab ID, listing identity, view kind, path, filter preset/pattern/query, hidden flag (plus selection digest only for the "Only selected" preset). Sort and focus are NOT part of it.
- A late provider response is resolved against the old snapshot: swap -> `pane_swapped`, navigation/relist/filter -> `context_changed`, tab closed or no longer active -> `tab_gone`; resolver returns `stale_context` and never retargets.
- `ContextFlow.revalidate` re-checks source (+ selection for `selected`) and target pane before execution. The frozen manifest from `entries(handle)` is unaffected by later pane changes.
- Cancelled requests: `resolveArchive(..., { cancelled })` returns `cancelled` before creating any manifest.

## Found while testing against the real `PaneStore`

- `PaneStore.tab` is `undefined` until `newTab`/`restore` has run (empty `tabs` initially); `buildPaneProbe` therefore returns `undefined` and `capture` throws `pane_unavailable`.
- Closing the last tab replaces `tabs[0]` with a new `TabState`, so its stable ID changes (a new tab), while navigation, back/forward and closing another tab keep the ID.

## Integrator handoff (not done here; shared files)

1. `src/lib/components/FilePane.svelte` (UI/integrator owned). In `onMount`, next to `registerPane(...)`, register the read-only probe and call the unregister in the cleanup:

```ts
import { buildPaneProbe, registerPaneContext } from '../ai/contextPanes';
const side = paneId === 0 ? 'left' : 'right';
const offCtx = registerPaneContext(
  side,
  buildPaneProbe(() => ({
    paneId: side,
    tab: store.tab,
    rows: store.rows,
    visible: () => visible,
    selected: () => selected,
    path: store.path,
    branchActive,
    inArchive,
    filterMode: view.filterMode,
    filterPattern: view.filterPattern,
    filterQuery: filterQ,
    showHidden: ui.showHidden,
    sort: { key: effSort.key, dir: effSort.dir },
  })),
);
```

`visible` is the existing derived list (already full, not viewport-limited). `searchActive` can be passed once a search-as-pane view exists. Note `hideSame` (compare "Hide Same names") is already inside `visible`. 2. Host for the service: `new ContextService(registryHost(() => (ui.activePane === 0 ? 'left' : 'right')))` created by the integrator/executor. 3. AI-02/05/06 consume `ScopeOk.handle`/`TargetOk` + `ContextFlow.revalidate`; the executor must call `revalidate` right before queueing and treat any failure as `stale`.

No change to `paneStore`/`panesRegistry` is required: stable tab IDs come from the tab object identity. If a persistent ID is wanted later (e.g. across session restore), add `id` to `TabState` in `makeTab` (integrator), then `tabIdOf` can read it.

## Limits and not tested

- The FilePane registration above is untested in the running app (no desktop/E2E run). Resolver behaviour is proven against the real `PaneStore` and the real sort/filter pipeline, but the Svelte component glue is not exercised.
- Symlink/alias root identity and case-insensitive volume semantics are left to the backend; `resolveSync` only compares normalised path strings (case-insensitive).
- The unique-name suggestion (`Name (2).zip`) is local; it must be reconciled with the existing Pack rename policy once Pack (F2-12) lands.
- "Hidden" exclusion counts assume visible entries are a subset of the listing.
- Per-pane `contextRevision`/`selectionRevision` counters advance only when `capture` observes a change; staleness itself is decided by direct comparison, not by the counters.
