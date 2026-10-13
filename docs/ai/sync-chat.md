# Workflow chat "sinkronkan kedua panel dua arah" (AI-11 / FER-168: FER-215..218)

Status: kode + tes. Adapter `directories.sync`, guard backend untuk sync, dan tes jalur penuh selesai di branch
`feature/ai-s2-chatsync`. **Skrip desktop `scripts/ai-desktop-smoke-sync.mjs` sudah ditulis ulang untuk jalur chat tetapi BELUM DIEKSEKUSI**
(hanya `node --check`). Pembukaan gate `ENGINE_GATES['directories.sync']` ada di commit terpisah terakhir (lihat "Gate").

## Alur (satu approval)

1. Pengguna: "sinkronkan kedua panel dua arah". Provider hanya mengusulkan `directories.sync` (kiri, kanan, `bidirectional`); tidak ada
   path, nama, keputusan overwrite/delete/konflik dari model.
2. `ContextFlow.resolveSync` mengambil root dari snapshot (folder biasa, berbeda, tidak bersarang). `buildSyncDryRun(l, r, {direction:'both'}, {byContent})`
   memanggil `validateSyncRoots` (alias symlink/junction ditolak berdasarkan path kanonik), compare (rekursif, default sama dengan panel manual),
   dan memuat baseline pasangan root itu sendiri (FER-414).
3. Tidak ada rencana bila: compare terpotong/dibatalkan/ada error baca, rencana memuat `delete` (ditolak dari chat), hanya konflik
   (`only_conflicts`), atau tidak ada yang perlu disalin (`nothing_to_sync`).
4. Preview (lokal, tidak dikirim ke model): root kiri/kanan, kiri→kanan N item (U menimpa), kanan→kiri M item (V menimpa), konflik (daftar,
   dilewati), hapus 0, dilewati/identik, total byte bila diketahui, `baselineStatus` (run pertama / baseline tersimpan / gagal dimuat).
   Daftar item dan konflik tersedia paginated lewat `SyncPreviewStore` (`listItems`, `conflictViews`). Konflik hanya DITAMPILKAN: chat tidak
   memutuskan konflik dan tidak ada `ConflictPort` yang dipasang.
5. Approval terikat pada digest dry-run (fakta per item: ukuran + mtime kedua sisi, aksi), kedua root, dan `baselineStatus`. Berubahnya folder,
   panel, atau baseline = digest lain = rencana baru. `ApprovalManager` menolak digest palsu (`stale`), tanpa tiket (`not_approved`).
6. Guard backend (lihat di bawah) di-register saat preview dibangun dan di-authorize tepat sebelum `runSyncPlan`.
7. `runSyncPlan(report, plan, {allowOverwrite: true})` (overwrite sudah tercantum di preview yang disetujui): validasi root lagi, compare SEGAR,
   cek stale, lalu `enqueueCopy` per arah/folder tujuan lewat queue yang ada. Konflik tidak pernah dieksekusi; tidak ada delete.
8. Setelah receipt `queued`, adapter memanggil `settleSyncRun({report, plan, result, readJob})` di latar, `readJob` dari queue port executor.
   Itu SATU-SATUNYA penulis baseline: menunggu job terminal, compare isi segar, `reconcileBaseline`, simpan atomik. Job gagal/dibatalkan/partial/belum selesai
   tidak menggeser baseline. Hasilnya tersedia lewat `onSettled(result, previewKey)` (tanpa nama/path) dan ditampilkan di kartu hasil (`run.settlement`, `describeSettlement`).
9. Hasil chat berasal dari job queue milik run: `Completed`/`Partially completed`/`Failed`/`Cancelled`. Batch yang gagal masuk antrean (`failedToQueue`)
   mengubah hasil `Completed` menjadi `Partially completed`; konflik yang dilewati muncul sebagai peringatan "N konflik dilewati dan tidak diubah"
   (tidak pernah "semua sinkron").

## Guard backend untuk sync (FER-216)

`src-tauri/src/ai/approval.rs` + `approval_commands.rs` diperluas tanpa mengubah perilaku ZIP (digest ZIP identik, semua tes lama utuh).

| Hal                               | Perilaku                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| --------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Register (saat preview)           | backend men-sidik-jari (jenis + ukuran + mtime) SEMUA path yang diberikan: berkas sumber, target yang akan ditimpa, direktori yang memuatnya, dan kedua root. Path target salinan BARU didaftarkan sebagai "harus tetap tidak ada" (`absentPaths`). Root kiri/kanan dikanonikalkan; sama/bersarang/alias = `invalid_roots`. Semua path harus berada di bawah salah satu root dan tanpa `..`. Batas 250.000 path. Tak terbaca = `source_unreadable` (fail closed). |
| Authorize (tepat sebelum enqueue) | jam backend, sekali pakai, digest/revisi/rencana cocok, KEDUA root kanonik sama dengan yang disetujui (root yang dialihkan junction = `root_mismatch`), scope digest (mengikat sumber + root kedua + daftar absen) cocok, tiap berkas masih sama (`source_changed`/`source_missing`), target baru masih tidak ada (`target_appeared`), kedua root masih ada dan tidak read-only.                                                                                  |
| Penolakan                         | `{kind:"stale", reasons:[kode], summary}`; tanpa path. Run menjadi `stale`, nol penulisan.                                                                                                                                                                                                                                                                                                                                                                        |
| Fail closed                       | `register` gagal / scope kosong / root plan tidak cocok scope / IPC error = tidak ada approval; `authorize` tanpa registrasi = ditolak.                                                                                                                                                                                                                                                                                                                           |

Tes Rust (direktori temporary, `cargo test --lib ai::approval`): sumber berubah, target yang akan ditimpa berubah, berkas baru muncul di target
yang disetujui (termasuk dangling link), berkas baru muncul di root, root dialihkan junction setelah preview, klaim dengan root salah/hilang,
digest scope palsu, sekali pakai, registrasi buruk (same/nested/alias/di luar root/`..`/tanpa root kedua/dengan output name/tak terbaca).

Batas jujur guard: sidik jari direktori hanyalah mtime-nya, jadi perubahan ISI berkas di dalam folder yang disalin utuh (entri folder
satu-sisi) tidak terdeteksi guard; yang menangkapnya adalah compare segar dan cek stale di `runSyncPlan`. Izin tulis hanya diperiksa
lewat atribut read-only. Batas ini dikunci oleh tes `sync_folder_fingerprint_sees_direct_children_but_not_deeper_edits`
(anak langsung baru menggeser mtime = stale; edit isi berkas di dalam folder tidak).

Keputusan (FER-382, F2): sidik jari TIDAK diperkuat, dengan alasan:

- Menghitung/mengukur anak langsung hanya menutup sebagian kasus (edit isi berkas anak). Edit di kedalaman tetap lolos, jadi
  menutup celahnya sungguhan butuh walk rekursif atas folder itu dua kali (register + authorize).
- Biaya tidak terbatas: root yang dilindungi bisa berisi 100k entri langsung; menambah `read_dir` + `stat` per entri pada kedua
  root di setiap register dan authorize memperlambat jalur approval, sedangkan menambah batas ukuran membuat semantiknya tidak konsisten.
- Dampak saat lolos kecil: folder satu-sisi disalin ke tujuan yang disetujui KOSONG (`absentPaths` menjaga target tetap tidak ada),
  jadi tidak ada data yang ditimpa; yang tersalin adalah isi sumber terkini. Overwrite berkas yang ditimpa selalu terlindungi
  sidik jari ukuran + mtime per berkas.
- Fingerprint juga ikut `scope_digest` (kontrak bersama); mengubah isinya mengubah digest dan fixture.

Delete: chat sync TIDAK PERNAH membuat job delete. Mode chat selalu dua arah; `buildSyncPlan` hanya menghasilkan `delete*` dari keputusan
konflik manual (`applyDecisions`) atau `deleteExtras` (bukan dua arah), dan adapter menolak rencana dengan `deletes > 0`
(`unsupported_plan`, tes "a plan that contains a delete is refused"). Jadi tidak ada hasil job delete yang perlu dilaporkan di kartu
hasil chat; `Completed` selalu berasal dari job copy milik run. Di panel manual, `runSyncPlan` hanya mengembalikan `deletes` dengan
status `awaitingConfirm` dan dialog delete (lane File Operations) yang mengeksekusinya; `settleSyncRun` tidak memberi outcome pada item
itu sehingga baseline tidak pernah menganggap delete berhasil. Bila kelak chat diizinkan menghapus, hasil job delete harus ditambahkan
ke `readJob`/kartu hasil lebih dulu (keputusan produk, bukan bagian pekerjaan ini).

## Bukti: nyata vs tiruan

- `adapters/sync.test.ts` (21 tes): adapter dengan IPC DIMOCK murni (compare/copy/baseline) — preview, penolakan, scope guard, run, stale, double dispatch.
- `adapters/syncChatFlow.test.ts` (18 tes): **NYATA** direktori temporary + kode TypeScript (facade, adapter, registry, ApprovalManager, AiExecutor,
  ledger memori, `createBackendGuard`, syncService/syncPlan/syncBaseline). **TIRUAN** berbasis filesystem: `compareDirs`, `validateSyncRoots` (realpath),
  `enqueueCopy` (copyFileSync + hasil job terskrip), penyimpanan baseline (JSON per pasangan), dan ledger approval backend (mimik TypeScript dari
  `approval.rs`). Bukan engine Rust; guard Rust dibuktikan oleh tes Rust di atas. Isi: first-run union lalu run kedua (modifikasi → overwrite
  tercantum, kedua-sisi-berubah dan hapus-sepihak → konflik tidak dieksekusi/tidak dihapus/tidak dipulihkan, baseline hanya bergerak untuk item
  terverifikasi), hasil chat == panel manual (pohon berkas, aksi, entri baseline), stale (berkas sumber, target baru, target overwrite, junction),
  double dispatch, approval palsu, root sama/bersarang/alias, compare terpotong, register backend gagal (fail closed), pane berpindah, job
  gagal/dibatalkan/partial, batch gagal masuk antrean, model tidak menerima nama/path.
- `backendGuard.test.ts`: kontrak TS ke command Tauri (ZIP tidak berubah, sync memakai kedua root + absen).
- Tidak teruji di sini: engine copy Rust (cancel/overwrite mengikuti FER-393, `sync_guarantees` tetap `#[ignore]`), queue Rust asli, UI desktop.

## Gate

`ENGINE_GATES['directories.sync']` dibuka di commit terpisah `feat(ai): open the directories.sync gate`. Maintainer dapat membatalkan commit
itu saja bila smoke desktop gagal; seluruh kode lain tetap aman karena registry menolak capability pending (tes `index.test.ts`).
Saat `ready`, capability muncul di `offeredCapabilityIds()` dan prompt provider; saat `pending` tidak.

## Batasan

- Resolve konflik: bila run `directories.sync` punya konflik, kartu rencana/hasil (`PlanCard`; tombol sudah tampil di preview yang menunjukkan konflik, sebelum approval, dan tidak menjalankan apa pun) menampilkan tombol "Resolve in Sync panel" yang memanggil
  `compareStore.openSync({left, right})` dengan `leftRoot`/`rightRoot` dari plan run itu. Panel Synchronize Dirs dibuka dengan compare + dry-run
  baru untuk pasangan itu (bukan pane aktif); pengguna memutuskan tiap konflik di sana lewat UI yang sudah ada. Argumen `openSync`: objek =
  pasang pasangan folder, `null` = kembali ke pane aktif/tak-aktif (aksi `commands.synchronize`), tanpa argumen = pertahankan pasangan sekarang
  (Refresh, toggle by-content). Pasangan di-reset saat panel ditutup. Tombol murni sisi UI: root tidak masuk teks/data ke provider.
  Tidak tampil untuk run tanpa konflik atau non-sync. Tes: `components/ai/PlanCardResolve.test.ts`. Belum dicoba di app desktop asli.
- Chat tidak memutuskan konflik, tidak menghapus, tidak mengubah mode (selalu dua arah) dan tidak membuka keputusan "keep both".
- Compare rekursif per berkas; folder satu-sisi = satu entri. Mode isi (`byContent`) default mati seperti panel manual.
- Overwrite/cancel di tengah salinan mengikuti engine; FER-393 (staging copy) sudah ter-merge dan `cancelled_overwrite_keeps_the_previous_target_content` aktif.
- Menutup aplikasi sebelum job selesai berarti baseline tidak diperbarui (aman).
- Skrip smoke desktop belum dieksekusi.

## Desktop smoke result (integrator)

`scripts/ai-desktop-smoke-sync.mjs` ran against the real Tauri app with the live provider. The first run found a real
bug: the baseline never recorded files copied by run 1, so run 2 listed them as `noBaseline` conflicts (4 instead of
2). Cause: the Rust compare's content phase only demotes a size-equal pair to `contentDifferent`; byte-identical
copies with another mtime stay `newerLeft`/`newerRight` (copies do not preserve mtime, FER-393), while
`reconcileBaseline` required `same`. Fix: `settleSyncRun` treats content-verified `newerX` as identical
(`verifiedByContent`); the test double in `syncBaselineFlow.test.ts` now behaves like Rust (6 tests fail without the
fix). After the fix the script passes every check (two runs, conflicts skipped, one-sided delete not propagated, stale
guard with the backend wording).
