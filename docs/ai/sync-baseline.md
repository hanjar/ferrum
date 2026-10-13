# Persistensi baseline sync dua arah (FER-414)

Tindak lanjut AI-10 / FER-167 (handoff H1-H4 di `docs/ai/sync-audit.md`). Syarat sebelum gate chat `directories.sync` (FER-168) boleh dibuka: tanpa baseline tersimpan, mode `both` selalu konflik.

## Penyimpanan (Rust, `src-tauri/src/sync_baseline.rs`)

- Lokasi: `<app data dir>/sync-baselines/<hash>.json`, satu file per pasangan root. `<hash>` = 128 bit pertama SHA-256 dari kunci pasangan.
- Kunci = path KANONIK terurut `L=<kiri>\nR=<kanan>` (di-lowercase pada Windows), dihitung setelah `compare::validate_sync_roots` lolos. Jadi alias symlink/junction, `.`/`..`, dan beda huruf besar-kecil menghasilkan kunci yang sama; panel yang ditukar = pasangan lain (first-run).
- Format (versi 1, JSON, DTO strict `deny_unknown_fields`): `{version, key, savedAtMs, baseline: {version, leftRoot, rightRoot, entries: {nama: {left: {size, modifiedMs}, right: {size, modifiedMs}}}}}`. `key` dicatat di dalam file agar file yang tertukar terdeteksi.
- Tulis ATOMIK: file sementara eksklusif `.tmp-<pid>-<seq>-<nama>` (`create_new`, nama yang sudah ada milik pihak lain tidak ditimpa/dihapus) + fsync + `rename` ke file milik pasangan itu sendiri. Crash saat menulis tidak merusak baseline lama; sisa sementara dibersihkan saat gagal.
- Batas: 200.000 entri per baseline, 48 MiB per file, 256 pasangan. Terlampaui => `save` ditolak dengan kode `limit_exceeded` dan baseline lama TIDAK diubah/dipotong (perilaku jujur; sisi TS melaporkan `saveFailed`).
- Load tidak menebak: tidak ada file => `none`; JSON rusak/terpotong/field asing => `corrupt`; versi lain => `unknownVersion`; kunci di file tak cocok => `mismatch`; root pindah/volume terlepas/tak terbaca => `rootsUnavailable`; file terlalu besar => `tooLarge`. Semuanya = first-run. Root yang sama/bersarang/alias DITOLAK (`invalid_roots`), bukan first-run.
- `save` menolak baseline yang bukan milik pasangan itu (roots tak cocok), versi salah, nama entri kosong/NUL/terlalu panjang.
- Error lintas IPC hanya `{code, message}` tetap, tanpa path.

## Command Tauri

`sync_baseline_load(pair)` -> `{status:'ok', baseline}` atau `{status:'firstRun', reason}`; `sync_baseline_save(pair, baseline)`; `sync_baseline_clear(pair)` (idempotent). `pair = {leftRoot, rightRoot}`. Registrasi: `pub mod sync_baseline;`, tiga baris command, dan state `SyncBaselineStore` di `setup` (`lib.rs`). Bridge TS: `src/lib/ipc/syncBaseline.ts` (di luar Tauri: load = first-run, save/clear = no-op).

## Alur TS

- **Muat**: `loadSyncBaseline(left,right)` (syncService) dipanggil oleh `buildSyncDryRun` (hanya untuk arah `both`, bila pemanggil tidak memberi `baseline`) dan oleh `SyncFlow.openSync`. Hasil non-tepercaya = `null` + `baselineStatus` (`trusted` | alasan first-run | `loadFailed`). Kegagalan baca tidak memblokir pratinjau.
- **Simpan hanya lewat jalur terverifikasi**: `reconcileBaseline` mengembalikan tipe bermerek `VerifiedBaseline`; `syncBaselineSave` hanya menerima tipe itu.
- **H3 (hasil job -> item)**: `runSyncPlan` kini mencatat `jobId` pada item `queued`. Setelah job selesai, `settleSyncRun` (dijalankan di latar oleh `SyncFlow.executeSync` bila di shell desktop): (1) menunggu job terminal (polling, batas 4 jam; belum selesai => tidak menyimpan, `jobsNotFinished`), (2) `mapJobOutcomes`: `ok` hanya bila job `completed` TANPA failure/error; job `failed`/partial (completed tetapi ada failure) => `failed`; `cancelled` => `cancelled`; item tanpa job (skip, konflik, hapus menunggu konfirmasi, gagal enqueue) tanpa outcome, (3) compare SEGAR mode isi (`validateSyncRoots` dipanggil lagi, `noCache`), (4) `reconcileBaseline(previous dimuat ulang dari disk, fresh, rencana yang dieksekusi, outcomes)`, (5) simpan atomik. Tidak ada perubahan => tidak menulis (`unchanged`).
- **Mengapa compare isi**: copy engine belum mempertahankan mtime (FER-393), jadi hanya byte yang membuktikan "identik". Konsekuensi di `syncPlan.ts` (`resolveBoth`): bila baseline mencatat identik dan KEDUA sisi tidak berubah sejak itu, perbedaan metadata yang tersisa (mtime hasil copy) = `skip`, bukan konflik. Tes lama "mtime jitter" disesuaikan (conflict -> skip) dengan alasan itu.
- **H1**: semua jalur memanggil `validateSyncRoots` lewat `compareForSync` (dry-run, re-validasi `runSyncPlan`, compare verifikasi `settleSyncRun`); root nested/same ditolak sebelum compare, load, atau save (teruji). `SyncFlow` memakai `compareForSync`; chat (FER-168) harus memakai `buildSyncDryRun`/`runSyncPlan`/`settleSyncRun`, bukan `compareDirs` langsung.
- **H4**: keputusan konflik (`keepLeft`/`keepRight`/`skip`) hanya mengubah rencana (`applyDecisions`); baseline bergerak hanya untuk item yang job-nya `ok` dan terverifikasi identik. `skip`, konflik tanpa keputusan, dan job gagal membiarkan entri apa adanya. "Keep both" TIDAK ada di rencana/UI saat ini (bukan bagian perubahan ini).

## Bukti (apa yang nyata, apa yang tiruan)

- Rust, filesystem nyata (`cargo test --lib sync_baseline` 20 tes; `cargo test --test sync_baseline_fs` 8 tes): compare + copy asli (`compare_dirs`, `run_copy`) pada direktori temporary; store asli ditulis ke disk dan dibuka ulang (restart); korup/versi/kunci/root pindah/terlalu besar => first-run; alias (dot-segments, junction Windows), nested/same ditolak; dua pasangan independen; atomik tanpa sisa sementara.
- TS, `src/lib/syncBaselineFlow.test.ts`: direktori temporary nyata (node:fs) tetapi IPC TIRUAN berbasis filesystem (`compareDirs`, `enqueueCopy`, `validateSyncRoots`, store JSON per pasangan): first-run union, both-changed, modify-vs-delete, no-delete default, job gagal/cancel/partial/belum selesai tidak menggeser baseline (byte-identik), save ditolak melaporkan `saveFailed`, keputusan konflik, korup/versi/tukar panel, dua pasangan, validasi root. Ini BUKAN engine Rust; engine Rust dibuktikan di tes Rust di atas.
- `src/lib/stores/syncFlowBaseline.test.ts`: SyncFlow dengan semua IPC dimock (murni mock).
- Skrip desktop `scripts/ai-desktop-smoke-sync.mjs`: kini mengikuti JALUR CHAT (FER-168, `docs/ai/sync-chat.md`); **belum dieksekusi** (hanya ditulis + `node --check`).

## Batasan

- Cakupan engine tidak berubah: compare rekursif per file; folder satu-sisi = satu entri.
- Cancel/izin/disk penuh di tengah overwrite tetap tidak dijamin sampai FER-393 (staging copy) ter-merge; test `cancelled_overwrite_keeps_the_previous_target_content` tetap `#[ignore]`.
- Item di job yang gagal sebagian tidak dicatat meski sebagian byte sudah tersalin; baseline baru bergerak pada run berikutnya bila terbukti identik. Konservatif, bukan keliru.
- Compare isi setelah run membaca byte semua file (cache isi tidak dipakai); mahal pada pohon besar.
- Menutup aplikasi sebelum job selesai berarti baseline tidak diperbarui (tetap yang lama); aman, run berikutnya menganggap sisa sebagai belum terbukti.
- Penyimpanan belum bisa dihapus dari UI (command `sync_baseline_clear` ada, belum ada tombol).

## Handoff FER-168 (chat `directories.sync`) — dikerjakan di `docs/ai/sync-chat.md`

1. Pakai `buildSyncDryRun(left, right, {direction:'both'}, {byContent})` (memuat baseline sendiri; `baselineStatus` menjelaskan first-run) lalu `runSyncPlan`; tolak rencana berisi `delete` seperti sebelumnya.
2. Setelah `runSyncPlan` berstatus `queued`, panggil `settleSyncRun({report, plan, result, readJob})` dengan `readJob` dari queue port eksekutor (`OperationProgress` per `jobId`); jangan menulis baseline lewat jalur lain.
3. Konflik tetap tidak dieksekusi tanpa keputusan pengguna; chat tidak boleh memutuskan konflik.
4. Pesan ke model: hanya hitungan (`baselineStatus`, jumlah konflik), tanpa nama/path.
