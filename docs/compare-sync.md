# Compare & Sync engine (FER-389, Rust lane)

Source: `src-tauri/src/compare.rs`. IPC contract (TS): `src/lib/ipc/compare.ts`.
Everything here is read-only; compare never writes to either tree.

## Commands

| Command                                                       | Notes                                       |
| ------------------------------------------------------------- | ------------------------------------------- |
| `compare_files(pathA, pathB, opId?, encoding?, diffContext?)` | byte-exact verdict + bounded text diff      |
| `compare_dirs(left, right, byContent, recursive?, opId?)`     | one level or recursive                      |
| `compare_cancel(opId)`                                        | cooperative; unknown/finished id is a no-op |
| `compare_cache_clear()`                                       | drops cached verdicts                       |

Progress: event `compare://progress` `{opId, phase: 'scan'|'content', done, total?, current?}`, throttled to ~100 ms (a final event per phase is always sent). `compare_files` reports bytes in the `content` phase; `compare_dirs` reports entries (scan, total unknown) then files (content, total known).

## FER-106 text diff

- Only when the files differ and both sides look like text; absent for binary.
- Bounds per side: 2000 lines and 256 KiB. When either side is cut, `truncated=true`; a line cut by the byte limit is dropped so it cannot show as a false difference.
- Algorithm: common prefix/suffix trimmed, LCS table on the middle (at most 2000x2000 u16 cells), hunks with `diffContext` lines (default 3, max 100).
- Encoding: `auto` checks BOM (UTF-16LE/BE, UTF-8), else NUL means binary, else valid UTF-8 -> `utf-8`, otherwise `latin1`. Explicit `utf-8|utf-16le|utf-16be|latin1` forces it (explicit utf-16 skips the NUL check). Decoding is manual (no `encoding_rs` dependency). If the two sides auto-detect different single-byte encodings both are decoded as `latin1`; UTF-16 vs other is reported as `"<left>+<right>"`.
- Line endings are compared exactly: CRLF vs LF shows as del/add pairs with identical shown text. A missing final newline alone is not shown as a hunk (files are still reported unequal by bytes).
- `leftLine/rightLine`: byte-exact for single-byte-newline encodings; for UTF-16 derived from the first changed line of the diff.

## FER-107 directory compare

- `recursive` descends only into directories present on BOTH sides. `entry.name` is a `/`-separated relative path. A directory present on one side is ONE entry (`isDir=true`, `leftOnly`/`rightOnly`), never expanded. Directories on both sides are `same` entries (`isDir=true`). Folder-vs-file with the same name is `sizeDifferent` with `isDir` unset.
- Symlinks/junctions: `symlink_metadata`, never followed (so no loops); they are plain entries (not `isDir`, not content-compared).
- Entries that cannot be read (stat failure, unreadable subdirectory, content read failure) go to `errors[{name,message}]` and are omitted from `entries` (state unknown, not guessed). A root that cannot be read is a hard error.
- Cap: 50 000 entries -> `truncated=true`. Recursion depth guard 128 (reported in `errors`).
- Same-size content check (`byContent`) runs after the scan in a `content` phase and uses the verdict cache. mtime tolerance 2 s (FAT).
- Cancelled -> `cancelled=true`, entries are partial (content-pending entries keep their metadata state). Consumers must not drive a sync from a cancelled report or one with `errors`.

## FER-109 cancel, cache

- `opId` registry (`op_id -> AtomicBool`) is cleaned when the operation ends (RAII guard). Cancel is checked per 64 KiB chunk and per entry.
- Verdict cache is in-process only, key `(pathA, pathB, sizeA, sizeB, mtimeA, mtimeB)` with ns mtime; any size/mtime change misses. Bounded to 4096 entries, FIFO eviction. Cancelled/failed compares are never cached. Limitation: an edit that preserves both size and mtime is not detected until the cache is cleared; if no reliable mtime is available the pair is not cached. Text diff and line numbers are always recomputed; `fromCache` means only the byte-stream pass was skipped.

## Archive members (FER-413, `compare_archive.rs`)

`compare_files` accepts archive members on either side (member vs member, member vs disk file), read in place and streamed in 64 KiB chunks. Nothing is extracted or written, to the destination or to a temp dir.

- Address: the existing string-path convention, an OS path whose ancestor is an existing archive FILE, e.g. `C:\x\a.zip\dir\inner.txt` (same shape as `Location::Archive { archivePath, memberPath }`; the part after the archive, joined with `/`, is parsed by the archive layer's own `MemberPath` grammar, so `..`, reserved names and drive letters are rejected as `InvalidPath`). A path that exists on disk is always treated as a plain file; only a missing path is tried as a member address.
- Supported: `.zip`, `.7z`/`.7zip`, through the public `ArchiveProvider::list` (size/type lookup, no member data) and `read` (bounded, typed errors). The push-style `read` is bridged to a pull `Read` by a worker thread and a 4-chunk bounded channel, so memory stays constant; dropping the reader (cancel, early mismatch) stops the worker.
- Unsupported, with an explicit reason in the `Unsupported` error: `.rar` (the RAR provider loads the whole member in memory, not a bounded stream), `.tar/.gz/.tgz/.bz2/.xz/.jar/.apk/.cbz` (no read provider), a file that is not really a zip/7z (bad signature), and encrypted members/headers (compare has no password channel).
- Limits: a member declared larger than 512 MiB (the archive layer's per-entry limit) is refused. The stream must match its declared size exactly: more bytes (lying header / zip bomb) or fewer bytes is an error, never a verdict. Corrupt archives, missing members and directory members return `Io`, `NotFound` and `NotADirectory`.
- Cancel and progress work as for disk files (cancel polled per chunk, `content` progress by bytes of the longer side). A cancelled or failed compare is never cached.
- Cache: the member key is `(archive path, archive size, member path)` with the archive mtime plus the member's declared size. Replacing the archive misses; if the archive mtime is unavailable the pair is not cached. Same limitation as disk: an edit that preserves archive size, mtime and member size is not detected until the cache is cleared.
- Costs and gaps: each pass re-opens the archive (a diff does up to three passes per side) and 7z decodes sequentially, so a member of a SOLID 7z re-decodes the preceding data. Nested archives (an archive inside an archive) are not addressable. `compare_dirs` and sync still work on disk trees only and do not descend into archives.

## Scan cepat untuk compare/sync (FER-472, `tree_source.rs`)

`compare_dirs` tidak lagi membaca filesystem sendiri. Walker (`compare::Walker`) meminta satu level folder ke sebuah
`TreeSource`, per sisi. Semantik compare (symlink/junction tidak diikuti, toleransi mtime 2 detik, `errors`, cancel,
cap 50 000 entri, kedalaman 128) dan baseline tidak berubah. Compare tetap read-only.

### Tingkat A: selalu aktif, tanpa admin (`DiskSource`)

- Satu `FindFirstFileExW` per folder dengan `FindExInfoBasic` + `FIND_FIRST_EX_LARGE_FETCH`. Ukuran, mtime, atribut, dan
  tag reparse langsung dari listing, jadi tidak ada `stat` per file. Path di atas MAX_PATH memakai bentuk `\?\`.
- Symlink/junction: atribut reparse + tag name-surrogate (aturan yang sama dengan `FileType::is_symlink` di std). Entri
  biasa, `isDir=false`, tidak pernah diikuti.
- Look-ahead paralel (`Prefetch`): sementara walker memproses folder secara berurutan, sampai 3 thread tambahan sudah
  membaca folder-folder berikutnya (kedua sisi sekaligus, hanya folder yang ada di KEDUA sisi, sama dengan yang
  di-descend walker). Urutan hasil, urutan `errors`, dan semantik cancel sama dengan walk berurutan (ada tes). Paling
  banyak 4 listing berjalan bersamaan, dan paling banyak 2000 listing selesai-tapi-belum-dipakai ditahan di memori.
- Root folder kosong tanpa `.`/`..` (root FAT/exFAT kosong) membuat Find gagal FILE_NOT_FOUND; itu dibaca sebagai folder
  kosong, bukan galat (review M1).
- Beda yang disengaja: mtime SEBUAH FOLDER di report kini berasal dari indeks folder induknya, yang di NTFS
  diperbarui malas dan bisa tertinggal beberapa saat dibanding `lstat`. Mtime folder tidak dipakai keputusan sync
  (folder hanya `same` atau satu-sisi), maka `planFacts` (cek plan basi) mengabaikan mtime folder. Mtime FILE tetap
  dihitung.

### Tingkat B: opsional, UAC (`MftSource`)

- Dipakai hanya bila user memilih "Scan cepat (admin)" di dialog tawaran, dan hanya untuk sisi yang berada di drive NTFS
  lokal (cek `fast_scan::eligibility`, sama dengan Disk Usage). Satu helper (`ferrum-scan-helper.exe`) dan satu UAC per
  volume berbeda. Kedua sisi di volume yang sama memakai satu scan.
- Hasil MFT difilter ke subtree root sync (lookup komponen path, tanpa case), lalu dilayani dari memori per folder.
  Aturan data tak tepercaya sama dengan Disk Usage (satu entri folder per referensi, orphan dibuang, tidak ada siklus).
- Ukuran = data size `$DATA`, mtime = waktu ubah `$STANDARD_INFORMATION` (bukan salinan di `$FILE_NAME`, yang basi).
  Ini membutuhkan perubahan protokol pipe, lihat di bawah.
- Fallback ke tingkat A (dengan `report.fastScan.note` yang ditampilkan di panel Sync): UAC ditolak, helper tidak
  terpasang, volume bukan NTFS lokal, root tidak ada di MFT, galat helper/protokol, atau ada record MFT rusak
  (`corrupt_records > 0`: listing mungkin tidak lengkap, sync tidak boleh dijalankan dari situ).
- (Review M2) Verifikasi live berlaku untuk KEDUA tingkat: indeks folder NTFS baru memperbarui ukuran/mtime saat penulis
  menutup file, jadi file yang sedang terbuka bisa tampak tidak berubah. Setiap entri yang dilaporkan berbeda dibaca ulang
  lewat handle (`lstat`) sebelum masuk report. Entri `same` tidak diverifikasi (tidak memicu aksi).
- MFT adalah snapshot. Setiap entri yang akan DILAPORKAN berbeda (bukan `same`) dibaca ulang dari filesystem live
  (`lstat`) di sisi MFT sebelum masuk report, sehingga ukuran/mtime/keberadaan yang dilaporkan selalu live. Batasan:
  file yang berubah SETELAH snapshot dan tampak `same` di snapshot tidak terlihat sampai scan berikutnya.
- Helper juga membaca folder yang ditolak ACL untuk user (lihat `disk-usage-contract.md` §7.2). Walker biasa akan
  mencatatnya sebagai error (sync diblokir); lewat MFT folder itu tampil normal, dan salinan ke/dari folder itu gagal per
  item. Ini beda yang diketahui.

### Alur UI dan keamanan eksekusi

- Dialog tawaran muncul saat panel Sync DIBUKA (bukan saat Refresh/toggle konten, yang memakai pilihan yang sama),
  hanya bila preferensi "Tawarkan scan cepat" aktif dan salah satu folder eligible. Tombol default/Escape/Enter =
  "Scan biasa". Preferensi itu dipakai bersama Disk Usage.
- Pra-eksekusi: `executeSync` selalu memvalidasi ulang dengan compare BARU. Validasi ulang itu SELALU tingkat A (tanpa
  UAC lagi, dan tidak bergantung pada snapshot MFT), jadi yang dieksekusi diverifikasi oleh scan biasa.

### Perubahan protokol helper (perlu review ulang keamanan)

`ferrum-ntfs-scan::protocol` naik ke VERSION 2: setiap entri `Batch` mendapat satu field `modified: u64` (FILETIME,
100 ns sejak 1601, 0 = tidak diketahui) setelah `allocated`, +8 byte per entri. Decoder menerima nilai u64 apa pun
(tidak ada semantik yang bisa dilanggar; konsumen mengonversi dengan aritmetika saturating, di bawah 1970 = 0).
Helper membaca `$STANDARD_INFORMATION` (resident, offset 0x08) di `parse_record`, tanpa akses baru dan tanpa
alokasi baru. Hello versi 1 ditolak (tes). Tidak ada perubahan di pipe, DACL, nonce, impersonation, argv, batas
frame, atau alur autentikasi.

### Titik abstraksi `TreeSource`

`trait TreeSource { list(rel) -> DirListing; tier(); prefetchable(); needs_verify() }`. Service FerrumIndexer nanti
cukup mengimplementasikan trait ini (tanpa UAC, `needs_verify` sesuai kesegaran indeksnya) dan dimasukkan lewat
`compare_dirs_with_sources`; walker tidak berubah.

### Verifikasi manual UAC (owner)

UAC tidak bisa diotomatisasi dari sesi agent. Langkah untuk owner (prasyarat sama dengan `disk-usage-contract.md` §7.5:
helper `ferrum-scan-helper.exe` di samping `ferrum.exe`, build debug atau instalasi di Program Files):

1. Siapkan dua folder di drive NTFS lokal (mis. `C:\data\a` dan `C:\data\b`, ratusan ribu file lebih baik) dengan
   sedikit perbedaan: satu file beda ukuran, satu beda mtime, satu hanya di satu sisi.
2. Buka Synchronize Dirs (palette Ctrl+K). Dialog "Scan lebih cepat?" harus muncul dengan kedua path, dan fokus ada di
   **Scan biasa**. Tekan Enter: preview muncul tanpa UAC, tanpa baris "Scan cepat (MFT)" di panel.
3. Tutup panel, buka lagi, pilih **Scan cepat (admin)**, jawab **Yes** di UAC. Panel harus menampilkan "Scan cepat
   (MFT) dipakai untuk kedua folder", dan daftar entri/ukuran/mtime harus sama dengan hasil langkah 2.
4. Ulangi, jawab **No** di UAC: preview tetap muncul (scan biasa) dengan catatan "izin admin ditolak".
5. Folder di drive FAT/exFAT atau UNC dan folder lain di NTFS: dialog tetap muncul (salah satu eligible), UAC hanya
   untuk volume NTFS, sisi lain memakai scan biasa (catatan "bukan NTFS lokal" bila relevan).
6. Tekan Cancel (di panel) selagi helper membaca: tidak ada `ferrum-scan-helper.exe` tertinggal di Task Manager, dan
   panel kembali tanpa plan.
7. Jalankan sync dari preview MFT: eksekusi harus melewati re-validasi tanpa UAC kedua. Ubah satu file di antara
   preview dan Run: plan ditandai basi (bukan dieksekusi).
8. Centang "Jangan tanya lagi": sync berikutnya langsung scan biasa. Settings, "Tawarkan scan cepat (Disk usage dan
   Sync)" mengembalikannya.
9. Task Manager, Details: kolom Elevated untuk `ferrum.exe` tetap No sepanjang langkah di atas.
