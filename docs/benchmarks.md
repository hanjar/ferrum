# Host-measured benchmarks

Numbers are host-specific (see each entry), not universal claims. Rerun the
commands yourself to reproduce.

## Phase-2 engine benches — FER-29 (29 September 2026)

Machine: Windows 11 host (10.0.26200, x64, NVMe), rustc 1.98.1 (msvc).
Run: `cargo test --release --test phase2_bench -- --ignored --nocapture
--test-threads=1` at branch `feature/win-view-completeness` commit
`affb9ca`. All four `#[ignore]`d bench tests pass (4 passed / 0 failed,
100.59 s total wall). Release build; wall clock; ±10% run-to-run.

| Operation                                   | Time    | Notes                                      |
| ------------------------------------------- | ------- | ------------------------------------------ |
| List ZIP, 10k members                       | 0.06 s  | central-directory read                     |
| List ZIP, 100k members                      | 0.85 s  | central-directory read                     |
| Pack ZIP, 5,000 × 1 KiB (50 dirs)           | 0.97 s  | deflate, staging + atomic rename           |
| Test ZIP, 5,000 × 1 KiB (full decode + CRC) | 0.08 s  |                                            |
| Extract ZIP, 5,000 × 1 KiB                  | 4.65 s  | F2-03 no-follow publish pipeline           |
| Pack 7z, 5,000 × 1 KiB                      | 8.76 s  | LZMA2                                      |
| Test 7z, 5,000 × 1 KiB                      | 9.62 s  | per-entry decode overhead dominates        |
| Pack ZIP, 200 MiB incompressible            | 4.78 s  | deflate on deterministic random data       |
| Test ZIP, 200 MiB                           | 0.14 s  |                                            |
| Extract ZIP, 200 MiB                        | 0.43 s  | publish pipeline                           |
| Pack 7z, 200 MiB                            | 58.48 s | single-threaded LZMA2                      |
| Test 7z, 200 MiB                            | 0.50 s  |                                            |
| Search `*.log`, 5,000-file tree             | 0.16 s  | 5,000 hits / 5,050 visited; 30 s cap armed |
| Branch view, 5,000 entries                  | 0.17 s  | flattened subtree                          |
| Artifact sizes                              | —       | small-files zip 5.5 MB, 7z 4.9 MB          |

Not benchmarked: RAR (no legally redistributable fixture on this host —
same honesty rule as the F2-28 pass), startup time, resident memory,
steady-state CPU, DOM rows under load (DOM rows stay statically bounded by
the FileList virtualizer, `src/lib/virtual.test.ts`).

## Virtualized file list — task 05

Run: `npm run build && npx vite preview --port 4173` then open
`http://localhost:4173/?bench=100000` (screenshot: `docs/screenshots/bench-100k.png`).

| Metric                      | 100k synthetic rows (Edge headless 153, WebView2 engine)                                                                                            |
| --------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| DOM rows rendered           | **29** (viewport 20 rows @26px + overscan)                                                                                                          |
| Worst single scroll handler | **0.72 ms** (budget: <16 ms/frame)                                                                                                                  |
| JS heap used                | **31 MB**                                                                                                                                           |
| 150-frame auto-scroll pass  | 3.2 s wall (~21 ms/frame incl. headless paint sync — headless frame pacing is not representative; the handler-time number is the meaningful signal) |

Guarantee enforced by unit tests (`src/lib/virtual.test.ts`,
`src/lib/components/FileList.test.ts`): rendered row count ≤ visible + 2×overscan + 1
regardless of dataset size; cursor rows scroll into view from both directions.

Known gap: real-device scroll jank (input latency) is not measurable headless —
recorded as a limitation for task 15 to close on host with devtools tracing.

## Rust listing — task 04

Run: `cd src-tauri && cargo run --release --example bench_listing -- 10000`

| Entries | Setup (create files) | list_dir   |
| ------- | -------------------- | ---------- |
| 10 000  | 2.2 s                | **169 ms** |
| 100 000 | 32.9 s               | **2.9 s**  |

Host: Windows 11, NVMe, rustc 1.98.1 (msvc). 100k listing cost is dominated by one
lstat per entry; acceptable for the pathological case, revisit if real directories
of that size show up in practice (task 15).

## Copy engine — task 15

Run: `cargo run --release --example bench_copy`

| Workload                            | Result                                       |
| ----------------------------------- | -------------------------------------------- |
| 1 × 256 MB file + 2000 × 1 KB files | **2001 files / 257 MB in 1.22 s ≈ 270 MB/s** |

Host: same as above. Byte-identity is asserted by unit tests, not just the
benchmark.
