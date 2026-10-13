# AI capability `diskUsage.query` (FER-466)

Read-only. Answers questions such as "paling gede itu apa" from the Disk Usage result that is on screen.

## Flow

1. The app offers `diskUsage.query` to the provider only while the Disk Usage workspace is open or still holds a result (`diskUsageWorkspacePort().available()`). The prompt text describes the capability only: no path, name, size or count.
2. The model replies with a strictly typed intent (`deny_unknown_fields` in Rust, exact-key parser in TS, shared cases in `contract-fixtures/ai/disk-usage-cases.json`):
   - `largestFiles { scope?: view|root, category?: video|image|audio|document|archive|other, limit?: 1..20 }`
   - `largestFolders { scope?: view|root, limit?: 1..20 }`
   - `summary {}`
3. The executor runs it locally with the existing IPC queries (`disk_usage_top_files`, `disk_usage_query_children`) or the stored scan stats, and ends the run `answered` with an app-built `DiskUsageAnswer`. There is no second provider turn and nothing from the scan is in any provider request (`OutboundContext` has only pane kinds and counts).
4. The card (`DiskUsageAnswerCard`) shows name, size, share of the scan root and the path relative to the scan root. Actions: Reveal (existing pane navigation), Copy path (clipboard), Open Disk Usage. No delete or move; those still go through the normal plan and confirm card.

## Honesty rules

- No usable scan: "Belum ada hasil Disk Usage. Jalankan scan dulu." (or "scan masih berjalan") plus an Open Disk Usage button.
- Top-N is labelled "N dari M" and "bukan semua"; folders list only direct children of the scope.
- Partial, cancelled and stale scans carry a note. Unknown allocated bytes are shown as "tidak tersedia", never replaced by logical bytes.
- A snapshot replaced or evicted while the query runs is reported, not painted.

## Scope

`view` is the folder the treemap is zoomed into (`store.zoomNodeId`); `root` is the scan root.
