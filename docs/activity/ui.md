# Activity UI (ACT-07 / FER-234)

Surfaces: sidebar (Recent/Frequent/Favorites + first-run notice), Activity page (modal; Titlebar clock
button, sidebar "Lihat semua Activity…", palette "Buka halaman Activity"), Settings (privacy section).
Privacy controls are one shared component (`ActivityPrivacyControls.svelte`).

## Control semantics (all distinct)

| Control                 | Scope                                              | Effect                                                                                                 | Preview              |
| ----------------------- | -------------------------------------------------- | ------------------------------------------------------------------------------------------------------ | -------------------- |
| Pause (`Catat ...` off) | all windows, persisted                             | no new writes; history kept                                                                            | no                   |
| Private session         | THIS window only, not persisted                    | no writes from this window; suggestions hidden here (sidebar); page shows stored history with a banner | no                   |
| Retention 7/30/90       | persisted (`activity-settings.json` beside the DB) | purge older episodes at the new cutoff; backend queries use it                                         | yes, when shortening |
| Hide suggestion         | one item, persisted                                | leaves Recent/Frequent/palette search; statistics kept; reversible                                     | no (nothing deleted) |
| Forget item             | one item                                           | removes its history/statistics and its hide flag                                                       | yes                  |
| Exclude folder          | subtree                                            | removes existing history under it, stops recording it                                                  | yes                  |
| Clear all               | everything                                         | removes all history; Favorites, files and settings stay                                                | yes                  |

Destructive scopes: `requestScope` -> read-only `activity_preview` (items, episodes, sample, favorites kept)
-> explicit Confirm. If the privacy epoch moved between preview and confirm (another window acted), the
preview is recomputed and the user must confirm again.

## Data and coverage

- `activity_overview(days 7|30|90, kind, sort)` scans retained rows via the store's paged API (<= 1000 per kind;
  `truncated` is reported). Daily chart = distinct items by latest-access UTC day, **not** clicks.
- Coverage label: only activity inside Ferrum, local, max N days retention, oldest retained item date.
  Never "all time" nor OS-wide.
- Disclosure: shown while recording is on until acknowledged once (persisted).

## Privacy epoch and multi-window

- Every mutating command broadcasts `activity://changed` (new state). Windows drop cached suggestions at
  once and re-read; states with an older epoch are ignored.
- Store refresh brackets list reads with two state reads; if the epoch moved, lists are discarded and re-read.
  A superseded refresh never paints. Page overview carries its epoch and is dropped if older than known.

## Service-level hide (store gap)

The store crate has an `activity_hidden` table but no read/write API, and retention has no setting column.
Both live in the service (`activity-settings.json`: retention, hidden ids, disclosure ack) and are applied as
a filter on recent/frequent/search. Moving them into the store is a follow-up for the store owner.

## Opening files (FER-249)

Double-click or Enter on a FILE in a pane opens it via the OS (`shellOpen(path, recordingActive)`); the receipt is recorded only after the OS accepts it, and private/paused windows pass `record=false`. Folders still navigate; search/palette results still reveal; selection/preview never records. A failed open shows the error dialog. Cancelling the palette (Esc) records nothing.

Private session exists only in the renderer (per window; the backend does not know about it), so every renderer path that writes a receipt (`shellOpen` from panes, Sidebar/palette favorites, Activity page) must pass `record = activity.recordingActive` explicitly; `shellOpen` has no default and the backend treats a missing `record` as `false` (fail-closed, FER-432).

### Hidden items limit (review N2)

The hidden list is capped at 5000 ids (FIFO). Above that, the oldest hidden items can be suggested again without any user action. This is far above normal use; it is documented here and in `set_hidden`.
