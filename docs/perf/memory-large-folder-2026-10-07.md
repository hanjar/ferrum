# Memory when opening a large folder (FER-455, ref FER-446)

Question: why does private memory grow by ~225 MB (baseline 2026-10-06) after opening a folder with 10,000 files, and
how much of that can the app remove? Method: `scripts/profile-mem.mjs` (new; per-process private/working set, renderer
JS heap after forced GC, DOM counters, heap-snapshot summary by constructor) and `scripts/measure-app.mjs --runs 5`.
Both ran on the same machine, same day, the base exe (`main @ 74c549e`) and the fixed exe back to back.
Other developer sessions were active, so idle floors differ by about 10 MB between runs; compare the _growth_.

## Findings (profile, 10k files, base exe)

| Process                          | idle private | 10k private | delta        | Cause                                                 |
| -------------------------------- | ------------ | ----------- | ------------ | ----------------------------------------------------- |
| ferrum.exe (Rust)                | 6.9 MB       | 8.0 MB      | **+1.1 MB**  | listing + IPC buffers: negligible, not worth touching |
| WebView2 renderer                | 43.2 MB      | 119.0 MB    | **+75.8 MB** | the listing in JS (below)                             |
| WebView2 gpu / browser / utility | ~180 MB      | ~175 MB     | about 0      | fixed WebView2 floor, outside app control             |

Renderer JS heap (after GC): 3.8 MB idle to **27.5 MB** loaded (+23.7 MB). Heap snapshot, loaded, self size:

| Object                                                          | count        | MB         |
| --------------------------------------------------------------- | ------------ | ---------- |
| `array (object elements)`                                       | 83,197       | 6.5        |
| `Object`                                                        | 147,021      | 5.2        |
| `array`                                                         | 22,656       | 2.5        |
| `Array`                                                         | 83,546       | 1.3        |
| closures `get/set/has/defineProperty/o` (Svelte proxy handlers) | ~20,250 each | ~2.7 total |
| `system / Context` scopes                                       | 20,242       | 0.8        |

Root cause: `PaneStore.tabs = $state<TabState[]>` deep-proxies everything assigned into it, so each of the 10,000
`Row` objects and each of their `FileEntry` objects got a Svelte `Proxy`, a handler with 4 closures, and a per-property
signal map (about 2 proxied objects per file, 20k proxies, 83k element arrays). Rows are only ever replaced wholesale
(`tab.rows = ...`), never mutated in place, so none of that reactivity was used. DOM is not a factor: 33 rendered rows
(virtualized), 3,004 DOM nodes in both cases. No duplicate listing store exists (branch view has its own rows only
while Ctrl+B is active, and had the same proxy cost, fixed the same way).

## Fix

`rows` are stored in an `Array` subclass (`rawRows` in `paneStore.svelte.ts`). Svelte only proxies exact
`Object`/`Array` prototypes, so the array and its rows/entries stay raw; assigning `tab.rows` is still reactive through
the tab proxy. Same helper for `branchStore` rows. About 10 lines, no behavior change. Regression test in
`paneStore.test.ts` (fails without the fix, `util.types.isProxy`).

## Result (measure-app, 5 runs, same session)

| Metric                                 | base         | fixed             | change                        |
| -------------------------------------- | ------------ | ----------------- | ----------------------------- |
| Renderer JS heap, 10k loaded (profile) | 27.5 MB      | 6.7 MB            | **-76%**                      |
| Renderer private, 10k loaded (profile) | 119.0 MB     | 79.5 MB           | -33%                          |
| Private, tree idle                     | 228.0 MB     | 237.8 MB          | noise (+10)                   |
| Private, tree after 10k files          | 290.7 MB     | 250.7 MB          | **-13.8%** (-40 MB)           |
| Private growth caused by 10k files     | +62.7 MB     | +12.9 MB          | **-79%**                      |
| Working set after 10k files            | 479.8 MB     | 443.3 MB          | -7.6%                         |
| Listing time (open folder to rows)     | 278 ms       | 234 ms            | no regression                 |
| Startup median / p95                   | 474 / 836 ms | 500 / 836 ms      | within run-to-run noise       |
| Idle CPU                               | 0.56%        | 0.94% of one core | noise (other sessions active) |

## About the 25% target

Against the absolute total, -25% is not reachable from app code: of ~250 MB private after opening 10k files, about
175 MB is the WebView2 floor (GPU process 110 MB, browser process 39 MB, utility/crashpad 21 MB) plus 8 MB for
ferrum.exe, which is the same at idle. What the app controls is the growth caused by the folder: it dropped 79%
(+62.7 MB to +12.9 MB), and the absolute total fell 13.8% from this change alone. The remaining +12.9 MB is the
10k row objects themselves (V8 heap ~3 MB, plus renderer overhead) and is proportionate to the data.
The older baseline (464 MB private after 10k) was taken on a busier host and a different window size, so only
same-session comparisons are meaningful.

Raw: `scripts/profile-mem.mjs <exe> --out f.json`, `scripts/measure-app.mjs <exe> --runs 5 --json f.json`.
