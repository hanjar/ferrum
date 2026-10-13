# Ferrum Disk Usage — Size Contract & Engine Audit (DSK-00 / FER-318)

Status: **kontrak v1** untuk DSK-02 (scanner), DSK-03 (query), DSK-04/05 (UI).
Sumber semantik: Disk Usage specification.
Dokumen ini AUDIT + KONTRAK — bukan implementasi.

## 1. Audit engine existing (DSK-00.1)

Diverifikasi langsung dari isi worktree `feature/disk-usage` @ `085a91e`
(pembacaan file penuh; commit hash branch ini adalah bukti konten;
tidak ada klaim SHA lain di luar `085a91e`).

### 1.1 `src-tauri/src/properties.rs` (544 baris, modul Tauri-free)

- `EntryProperties` — detail satu entri: `path`, `kind` (`file|dir|symlink|other`),
  `size` (logical, dir = 0), `allocated_bytes`, `allocated_supported`,
  timestamps (`created/modified/accessed_ms`), `read_only/hidden/system`,
  `symlink_target` (dibaca tanpa follow).
- `OccupiedReport` — agregat rekursif: `files`, `dirs`, `symlinks`,
  `logical_bytes`, `allocated_bytes`, `allocated_supported`, `failed: Vec<FailedPath>`
  (`path`, `code`, `message`).
- `occupied_space(paths)` — walker **iteratif** (explicit stack, bebas stack
  overflow), `symlink_metadata` (link tidak pernah diikuti — D07; loop symlink
  mustahil), failure per-entri dilaporkan lalu lanjut (permission denied ≠ 0),
  logical hanya menjumlah file, allocated via `GetCompressedFileSizeW`
  (Windows) / logical+flag `false` (non-Windows), flag `allocated_supported`
  di-AND antar file (**all-or-nothing** — lihat gap G2).
- `volume_info(path)` — `GetVolumeInformationW` + `GetDiskFreeSpaceExW`
  (Windows-only; platform lain `Unsupported` — jujur, sesuai D44 Windows-first);
  `drive_root_of` hanya kenal `X:\` (UNC belum didukung).
- `set_attributes` — toggle READONLY/HIDDEN Windows, atribut lain dipreservasi.
- **12 unit test** in-file (`#[cfg(test)]`): ukuran/kind/waktu, dir=0,
  symlink-target tanpa follow (skip bila unprivileged), missing→NotFound,
  walk logical+allocated (1500 B fixture, allocated ≥ logical), loop-safety
  symlink, failure-and-continue, agregasi multi-root, volume C:\ (host),
  tolak path tanpa drive, round-trip atribut, atribut missing→NotFound.

### 1.2 `src-tauri/src/ipc.rs` (command surface, baris 737–779)

- `entry_properties(path)` (L739), `occupied_space(paths)` (L750),
  `volume_info(path)` (L763), `set_attributes(...)` (L772) — semuanya
  `spawn_blocking` + `validate_path`, kontrak disinkronkan dengan
  `src/lib/ipc/types.ts` + `docs/ipc.md`.
- `occupied_space` **single-shot**: tanpa progress event, tanpa cancel, tanpa
  scanId; dialog yang ditutup cukup membuang hasil read-only.
- Infrastruktur reusable yang SUDAH ADA dan menjadi pola wajib DSK-02:
  - `search_start` / `search_cancel` (L556/618): registry cancel per-id +
    streaming event `search://progress` (batch + `done` + `stats`/`error`) —
    pola event+cancel+id persis yang dibutuhkan scan progresif.
  - `OperationQueue` (`enqueue_retryable`, `cancel`, `snapshot`, `retry`)
    untuk pekerjaan berat yang butuh progress/cancel di BTM.
  - `list_branch` (F2-23, L784): listing flat subtree tercap `max_entries`
    (cek ulang apakah membawa size saat DSK-03 — belum diaudit di sini).

### 1.3 Bukti handoff FER-22

FER-22 (F2-21) status selesai **terbukti benar** untuk modul ini:
file, fungsi, dan tes di atas ada dan koheren. Ini BUKAN jaminan untuk
kebutuhan Disk Usage (lihat gap). Integrasi `cfb84d5` (klaim handoff) lebih
lama dari `085a91e` sehingga isinya tercakup; SHA per-file tidak diverifikasi
terpisah (keterbatasan alat, lihat §4).

## 2. Kontrak ukuran (DSK-00.2) — BINDING untuk DSK-02/03/04/05

1. **Logical bytes** = penjumlahan panjang konten menurut metadata (`st_len`)
   per pathname file. Footprint pathname: hardlink dihitung di setiap path
   (boleh dobel). Directory entry tidak menambah byte (dir = 0).
2. **Allocated bytes** = alokasi on-disk yang dilaporkan OS (`GetCompressedFileSizeW`
   di Windows). Ketersediaan **per entri/per node**, bukan all-or-nothing:
   - Node dengan allocated tersedia → tampilkan angka.
   - Tidak tersedia → `allocated_bytes: null` di API/UI + label
     "unavailable"; **DILARANG** mengisi dengan logical tanpa label.
   - Sebagian tersedia → coverage `partial`, label eksplisit.
3. **Bukan janji recoverable space.** Logical/allocated tidak boleh
   disajikan sebagai "ruang yang bisa dibebaskan". Hardlink, compression,
   sparse, reflink, recycle bin membuat pembebasan nyata berbeda. Tidak ada
   dedup reflink/shared extents.
4. **Unique-allocation mode (opsional, DSK-02 boleh menunda):** dedup hanya
   by verified volume+file identity (mis. file index + volume serial Windows)
   dalam scope satu scan; atribusi deterministic ke satu owner path; link
   lain `shared` dengan 0 attributed. Dilarang menjumlah semua hardlink lalu
   menyebutnya unique disk usage.
5. **Disk capacity/free** adalah metrik volume terpisah (`volume_info`);
   dilarang menghitung free space sebagai pengurangan total scan. Delete ke
   recycle bin belum tentu membebaskan ruang.
6. **Tampilan:** internal integer bytes (Rust `u64`); JSON/TS number aman
   sampai `Number.MAX_SAFE_INTEGER` (2^53−1 B ≈ 8 PiB) — v1 pakai number +
   guard komentar kontrak; display GiB/MiB basis 1024, pembulatan konsisten.
7. **Persen:** `percent-of-parent` dan `percent-of-scan-root` dihitung
   terpisah, denominator eksplisit; denominator 0 / coverage partial
   menghasilkan label "—" bukan 0%.

## 3. Kontrak scope & scan (DSK-00.3 / DSK-00.4) — BINDING

1. **Scope:** root lokal yang dipilih user. Hidden/system **disertakan**
   bila readable. Exclusions (jika ditambahkan) selalu tampil di UI.
2. **Symlink/junction:** dihitung sebagai entri, **tidak pernah diikuti**
   (mempertahankan D07; loop mustahil). Mounted volume lain / cross-volume
   **tidak dilintasi** secara default — walker DSK-02 wajib menambah guard
   boundary volume (walker sekarang belum mengeceknya — gap G3).
3. **Metadata-only:** tidak membaca payload file, tidak mengekstrak arsip,
   tidak meng-hydrate cloud placeholder. Read-only total — tidak ada mutasi.
4. **Progresif & generation:** setiap scan punya `scanId` unik dan
   generation guard; event progresif berisi `scanned_files`, `scanned_dirs`,
   `known_bytes` (logical ± allocated sesuai ketersediaan), `coverage`,
   `errors[]` batch — memakai pola event `search://progress`
   (`search_start`/`search_cancel`). Response yang terlambat dari scan lama
   **harus dibuang** bila generation sudah berganti. Cancel → state
   `cancelled`, BUKAN complete.
5. **Coverage:** enum `complete | partial | stale` + penyebab
   (errors, permission denied, deleted-during-scan, changed-during-scan).
   Scan adalah best-effort snapshot non-atomik; `partial` dan `cancelled`
   tidak boleh dilabel `complete`. Cached scan menyimpan waktu dan stale
   marker. Tidak ada persentase-selesai atau ETA selama total belum diketahui. Pengecualian
   FER-460: engine `ntfsMft` menampilkan persen fase **baca MFT** (`mftRecordsDone/mftRecordsTotal`), karena jumlah
   record MFT diketahui sejak awal. Fase menyusun tree tampil tanpa persen, dan tidak ada ETA.
6. **Error:** permission denied ≠ ukuran nol (polanya sudah benar di
   `OccupiedReport.failed` — pertahankan, tambah agregasi per node).
   Offline/unreachable → error state jujur, bukan 0 bytes.
7. **Subtree identity:** node id stabil per scan (path-derived), child
   folder total = direct files + seluruh child subtrees; **parent tidak
   pernah dijumlahkan lagi** ke total root. Root total = jumlah leaf, bukan
   jumlah baris tree.
8. **Query (DSK-03):** children largest-first paginated, top-N files
   rekursif per subtree, tie-break stable id, snapshot/cursor per scanId;
   selection bertahan saat progress; label "Top N" bukan klaim semua hasil.

## 4. Gap terhadap engine existing (input DSK-02/03)

| #   | Kondisi sekarang                                                      | Yang dibutuhkan Disk Usage                                       | Task                   |
| --- | --------------------------------------------------------------------- | ---------------------------------------------------------------- | ---------------------- |
| G1  | `occupied_space` single-shot agregat, hasil dibuang saat dialog tutup | Scan progresif per-subtree + langganan event + scanId/generation | DSK-02                 |
| G2  | `allocated_supported` all-or-nothing (AND seluruh file)               | Ketersediaan per node; `null` semantics + partial label          | DSK-02                 |
| G3  | Walker tidak memeriksa boundary volume                                | Guard anti lintas-volume mount                                   | DSK-02                 |
| G4  | Tidak ada cancel/progress pada occupied_space                         | Cancel + batch progress (reuse pola search)                      | DSK-02                 |
| G5  | Tidak ada agregat per node / tree                                     | Subtree aggregates tanpa double-count                            | DSK-02                 |
| G6  | Tidak ada hardlink identity (volume+file id)                          | Optional unique mode + deterministic owner                       | DSK-02 (boleh ditunda) |
| G7  | Tidak ada query top-N/pagination/sort/metric selector                 | Query snapshot/cursor largest-first                              | DSK-03                 |
| G8  | Tidak ada state stale/cached scan                                     | Waktu scan + stale marker + rescan                               | DSK-03/04              |
| G9  | `volume_info` Windows-only, UNC tidak dikenal                         | Cukup untuk Windows-first (D44); nyatakan limitasi di UI         | catatan                |

Yang SUDAH benar dan dipertahankan: iteratif stack (anti stack-overflow),
no-follow symlink D07, failure-and-continue, metadata-only read-only,
validate_path di boundary IPC, `spawn_blocking` agar UI tidak macet.

## 5. Keterbatasan audit ini (jujur)

- Gate `cargo test properties` TIDAK dijalankan saat audit: 12 tes diaudit statis dari
  source. DSK-02 wajib menjalankan suite properties sebagai baseline sebelum
  mengubah apa pun.
- `git log --follow` per file belum dieksekusi; kebenaran
  konten terikat pada HEAD `085a91e` yang diverifikasi bersih saat worktree
  dibuat.
- `list_branch` (F2-23) dan TS bridge `src/lib/ipc/types.ts` belum diaudit
  baris-per-baris; keduanya dicek ulang di awal DSK-02/03.

---

## 6. Support matrix, limitasi & bukti akhir (DSK-08)

### 6.1 Support matrix

| Aspek                                     | Status                                                                                                                                                       |
| ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Windows 10/11 x64, drive lokal NTFS/exFAT | Didukung penuh (allocated via `GetCompressedFileSizeW`)                                                                                                      |
| Platform non-Windows                      | Scan logical jalan; **allocated tidak tersedia** (`null` + "unavailable", jujur — bukan disamarkan); `volume_info` `Unsupported`                             |
| UNC path / mounted folder sebagai root    | Jalan sebagai scan biasa, TAPI **guard cross-volume hanya mengenali prefix `X:\`** — boundary di dalam UNC/mounted-folder tidak terdeteksi (G9)              |
| Symlink/junction                          | Dihitung sebagai entri, tidak pernah diikuti (D07); loop mustahil                                                                                            |
| Hardlink                                  | v1 = footprint pathname (boleh terhitung 2×); **unique-dedup ditunda** (G6) — tidak pernah diklaim "unique disk usage"                                       |
| Stale/cached scan                         | `Coverage::Stale` dicadangkan; invalidasi manual via mutasi in-app (delete/move/rename → chip Stale + Rescan). Mutasi eksternal (di luar app) belum ter-hook |
| Cloud placeholder / arsip                 | Metadata-only — tidak membaca payload, tidak hydrate, tidak membongkar arsip                                                                                 |

### 6.2 Error & exclusion coverage

Permission denied / deleted-during-scan / entry unreadable → error per path
(agregat, bounded 200 + `total_errors`) dan coverage `partial`; tidak pernah
dihitung 0. Hidden/system disertakan bila readable. Cancel → `cancelled`
(bukan complete). Tanpa persen-selesai/ETA saat total belum diketahui.

### 6.3 Limitasi disk-free (WAJIB dipahami pengguna)

Free space adalah **metrik volume terpisah** (`volume_info`). Jangan
mengurangkan total scan untuk menghitung free space. Delete ke recycle bin
belum tentu membebaskan ruang; hardlink/kompresi/sparse membuat ruang yang
terbebas berbeda dari ukuran logis. UI menyatakan ini di catatan metric (DSK-06.4).

### 6.4 Bukti akhir (ringkas)

- Suite (setelah DSK-09): `cargo test` **280 passed / 0 failed** (15 tes
  DSK-02 + 12 DSK-03 + 3 tes pool DSK-09: parity serial-vs-8-worker
  node-per-node, cancel mid-scan tanpa zombie, konsistensi coverage/counts
  di bawah depth cap); `vitest` **74 file / 688 passed**; `cargo fmt` /
  `clippy -D warnings` bersih. Coverage region `disk_usage.rs` 92.74%
  (llvm-cov, diukur saat DSK-08 — sebelum penambahan pool).
- Benchmark scan (host i5-13600K, NVMe, Win11; serial = walker
  single-thread DSK-07 verbatim, pool = DSK-09 `default_worker_count()` =
  16; kedua engine dijalankan berurutan dalam SATU proses di atas pohon
  yang sama, serial lebih dulu — cache hang justru menguntungkan pool):

  | Scan | Serial (before)         | Pool 16 worker (after)  | Speedup |
  | ---- | ----------------------- | ----------------------- | ------- |
  | 100k | 5.50 s (18.400 entri/s) | 1.79 s (56.393 entri/s) | 3.06×   |
  | 1M   | 41.71 s (24.243/s)      | 21.32 s (47.422/s)      | 1.96×   |

  Referensi silang ke baseline DSK-07 (host sama, sesi lebih sepi): 3,34 s
  @100k / 36,17 s @1M — terhadap angka itu pool = 1,87× / 1,70×. Cancel
  latency (pool) 0,24 ms; memori Δ proses 32,6 MB @100k (naik wajar: stack
  - antrean 16 worker, sebelumnya 21,3 MB). Speedup 1M < 100k: kemungkinan
    tekanan cache metadata NTFS pada pohon jutaan entri — diukur apa adanya.

- Limitasi kecepatan: diukur pada NVMe dengan metadata hang; pada HDD
  single-spindle atau saat filter AV membatasi I/O, ceiling paralel bisa
  jauh di bawah angka ini. Query snapshot tidak berubah kinerjanya (baca
  murni dari store).
- Temuan QA DSK-07 (B1 percent double-scale, B2 crumb off-by-one, B3
  metric-switch tree cache) sudah diperbaiki + regresi test (commit `82ca178`).
- Belum diuji: permission-denied ACL nyata (Windows unprivileged),
  sparse/WOF unit, overflow u64 eksplisit, OS lain.

### 6.5 Urutan integrasi ke main

1. Pastikan worktree main bersih (pastikan tidak ada perubahan pending).
2. `git checkout main && git merge --no-ff feature/disk-usage` di worktree
   `ferrum/` (10 commit fitur, linear di atas `085a91e`).
3. Gate gabungan ulang di main: `cargo fmt --check && cargo clippy
--all-targets -- -D warnings && cargo test` + `npx vitest run` +
   `npm run check` + `npx playwright test`.
4. Jangan hapus worktree `ferrum-disk-usage` sebelum gate gabungan hijau.

---

## 7. Scan cepat NTFS lewat helper admin (FER-460)

Status: **implementasi v1**. Ferrum tetap berjalan sebagai user biasa. Hanya `ferrum-scan-helper.exe` yang
dijalankan dengan hak admin, lewat UAC, dan hanya untuk satu scan.

### 7.1 Riset: MFT mentah vs `FSCTL_ENUM_USN_DATA`

| Aspek                      | Parse MFT mentah (`\\.\X:` + `FSCTL_GET_NTFS_VOLUME_DATA`)                                    | `FSCTL_ENUM_USN_DATA` (+ `FSCTL_GET_NTFS_FILE_RECORD`)                                           |
| -------------------------- | --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| I/O                        | Satu bacaan berurutan atas `$MFT` (≈1 KiB per record), dalam potongan 4 MiB                   | Enumerasi cepat, tetapi **tanpa ukuran**. Ukuran butuh satu ioctl/open per file (jutaan syscall) |
| Ukuran logical             | `$DATA` tanpa nama: data size                                                                 | Hanya lewat ioctl per file                                                                       |
| Alokasi                    | Header atribut: data size / total compressed (sparse/compressed) / stream `WofCompressedData` | Sama, tetapi per file                                                                            |
| Hardlink                   | Semua `$FILE_NAME` (kecuali 8.3) ada di record, jadi satu entri per nama                      | USN enum hanya memberi satu nama per record                                                      |
| Reparse (symlink/junction) | Tag dari `$REPARSE_POINT`, sama dengan aturan `is_symlink` walker                             | Atribut ada, tag tidak                                                                           |
| Kompleksitas               | Fixup update-sequence, runlist, record ekstensi/`$ATTRIBUTE_LIST`. ±600 baris, teruji fixture | Lebih sederhana kalau hanya nama, tetapi jalur ukuran membuat total kerja lebih besar dan lambat |

**Pilihan: parse MFT mentah.** Hanya cara ini yang memberi nama, semua hardlink, ukuran, dan tag reparse dalam
satu lintasan, sehingga C: dengan 4,5 juta file bisa selesai dalam hitungan detik (seperti WizTree). Crate `ntfs`
(Colin Finck, MIT/Apache) dan `mft` (omerbenamram) sudah dipertimbangkan, tetapi ditolak karena binary ini berjalan
sebagai admin: permukaan kode dan dependensinya harus sekecil mungkin dan mudah direview. Parser sendiri ada di
`src-tauri/crates/ntfs-scan/src/mft.rs`, dengan satu-satunya dependensi `windows-sys` (Microsoft, MIT/Apache-2.0).

### 7.2 Semantik (paritas dengan walker)

- Logical = data size `$DATA` tanpa nama. ADS diabaikan, sama seperti `st_len`.
- Allocated = nilai yang dilaporkan `GetCompressedFileSizeW`: ukuran data untuk file biasa, total compressed/allocated
  untuk stream sparse/compressed, dan stream `WofCompressedData` untuk file WOF/CompactOS.
- Reparse name-surrogate (symlink, junction, mount point) dicatat sebagai entri symlink: dihitung, tidak diikuti (D07),
  0 byte, allocated `null`. Reparse lain (cloud placeholder, dedup, WOF) tetap file/folder.
- Hardlink: satu entri per nama (footprint, §2.1). Nama DOS 8.3 dilewati.
- Record 0–15 (metafile `$MFT`, `$Extend`, …) tidak pernah dikirim. Anak `$Extend` menjadi orphan lalu dibuang,
  sehingga yang tampil sama dengan yang bisa dilihat walker.
- Root bisa drive (`C:\`) atau subfolder. Subfolder dicari per komponen, case-insensitive. Kalau root tidak ketemu,
  scan fallback ke walker.
- Snapshot dibangun oleh `disk_usage::fast::build_outcome` lewat `LiveState` + `finalize_scan` yang sama dengan walker,
  jadi id node, query, treemap, dan aksi tidak berubah. Ada tes paritas per node terhadap walker.
- Record MFT yang rusak dihitung, lalu coverage menjadi `partial` dengan satu error.
- **Beda yang disengaja:** folder yang ditolak ACL untuk user (mis. `System Volume Information`) tetap terhitung karena
  helper membaca MFT sebagai admin, sedangkan walker mencatatnya sebagai error. Nama file milik user lain juga ikut
  terlihat. Karena itu scan cepat dibatasi ke akun administrator yang meng-elevate **dirinya sendiri** (§7.3 butir 4):
  yang menerima daftar itu memang akun yang menyetujui UAC. Nama yang tidak valid untuk Win32 (mengandung `:` atau
  karakter kontrol) dilewati, tidak ditampilkan, tetapi dihitung bersama record rusak, sehingga coverage menjadi
  `partial` (§7.3 butir 7).

### 7.3 Desain keamanan

1. **Path helper** di-resolve dari direktori exe Ferrum (`current_exe()`), bukan dari PATH atau CWD. Bundle MSI/NSIS
   memasang helper di samping `ferrum.exe` (terverifikasi di `main.wxs` dan `installer.nsi` hasil `tauri build`).
   Build release hanya memakai helper bila direktorinya berada di Program Files (Known-Folder API, dibandingkan per
   komponen). Instalasi per-user (default NSIS `currentUser` di `%LOCALAPPDATA%`) bisa ditulis proses apa pun milik
   user, sehingga malware non-admin bisa menukar binary lalu menumpang pada "Yes" UAC berikutnya. Instalasi seperti
   itu tidak pernah ditawari scan cepat. MSI selalu per-machine, dan NSIS di-set `installMode: "perMachine"`
   (keputusan desain 2026-10-07), sehingga kedua installer memasang ke Program Files. Aturan lokasi tetap menjadi
   lapis kedua; instalasi NSIS lama yang per-user dijelaskan di `docs/release/rc1/UPGRADE-NOTES.md`. Build debug
   (dev) hanya melewati aturan lokasi ini kalau developer menyalakan `FERRUM_DEV_HELPER=1` (FER-463 L3). Tanpa itu,
   helper di `target\debug` yang bisa ditulis user tidak dipakai. Build release tidak pernah melewatinya. Signing: FER-445.
2. **Argumen** hanya `--volume X --pipe ferrum-scan-<32 hex> --nonce <64 hex>`, dengan urutan persis itu (`args.rs`).
   Argumen lain ditolak sebelum handle apa pun dibuka. Prefix `\\.\pipe\` ditambahkan oleh helper sendiri.
3. **Pipe** dibuat Ferrum sebelum helper berjalan: `FILE_FLAG_FIRST_PIPE_INSTANCE`, 1 instance,
   `PIPE_REJECT_REMOTE_CLIENTS`, nama acak 128-bit per scan, dan DACL terproteksi `D:P(A;;GA;;;<SID user>)`. Hanya SID
   user saat ini yang diberi akses, tanpa ACE lain (review W1 mencabut ACE BA).
4. **Akun yang sama saja (review W1).** Scan cepat hanya ditawarkan kalau akun Ferrum bisa meng-elevate dirinya sendiri:
   token split admin (`TokenElevationTypeLimited`) atau proses yang sudah elevated. User standar tidak ditawari, karena
   UAC-nya akan meminta kredensial akun lain, dan daftar file seluruh volume milik akun itu tidak boleh sampai ke user
   standar. Lapis kedua ada di helper: helper membandingkan SID user token-nya dengan SID token proses server pipe, lalu
   menolak kalau berbeda. Kalau tetap terjadi, Ferrum fallback ke scan biasa.
5. **Autentikasi klien lewat token, sebelum Ferrum mengirim apa pun (review W3).** Helper mengirim `ClientHello` yang
   kosong (magic + versi), karena server pipe harus membaca dulu sebelum bisa meng-impersonate. Ferrum lalu menjalankan
   `ImpersonateNamedPipeClient` (level identification) → `OpenThreadToken` → `RevertToSelf` (gagal = abort), lalu
   mewajibkan token klien **elevated** dengan **SID user yang sama**. Selain itu PID klien harus sama dengan PID dari
   `ShellExecuteEx`. Proses medium-IL milik user yang sama tidak bisa memalsukan token elevated. Setelah itu baru
   `ServerHello{nonce}` dikirim. Helper memeriksa direktori image server dan SID-nya **sebelum** menulis, membandingkan
   nonce dari argv (constant time) sebelum mengirim data, dan connect dengan `SECURITY_IDENTIFICATION`, sehingga token
   admin-nya tidak bisa dipakai server. Nonce mengautentikasi **server** terhadap helper. Klien diautentikasi oleh
   token dan PID, bukan oleh nonce.
6. **Read-only.** Volume dibuka dengan `GENERIC_READ`. Hanya record MFT yang dibaca. Isi file kecil yang resident
   memang ikut terbaca ke memori helper sebagai bagian dari record, tetapi tidak pernah dikirim (review I4). Tidak ada
   tulis, jaringan, proses anak, maupun file sementara.
7. **Frame berbatas** (`protocol.rs`): `len u32 + kind u8 + payload`, maksimal 1 MiB dan 16.384 entri per batch. Nama
   1–255 unit UTF-16, tanpa `\`, `/`, `:`, NUL, atau karakter kontrol 0x01–0x1F, dan bukan `.`/`..`. Larangan `:`
   mencegah id `C:\dir\a:b` mengarahkan aksi file ke ADS `b` milik `a` (review L6); helper juga tidak mengirim nama
   seperti itu. Enum dicek, trailing byte ditolak, dan count dicek sebelum alokasi. Ferrum membatasi total entri ke
   `min(40 juta, 4 × jumlah record MFT)` dan total nama ke 1 GiB, dengan pesan penolakan yang jelas (review L5).
8. **Parser tahan volume rusak (review W2).** Setiap run `$MFT` divalidasi dengan aritmetika checked: `vcn+clusters`,
   `× bytes per cluster`, dan `lcn+clusters ≤ TotalClusters`. `MftStream::read` memakai aritmetika checked dan menolak
   langkah tanpa kemajuan (`n == 0`), sehingga volume NTFS rakitan (USB) menghasilkan galat Layout, bukan loop atau
   bacaan di offset yang salah.
9. **Umur proses.** Helper keluar setelah `Done` atau `Error`, dengan watchdog 10 menit dan batas connect 30 detik.
   Cancel dari UI menutup pipe; write berikutnya di helper gagal sehingga helper keluar. Di sisi Ferrum: timeout
   connect 30 detik, timeout inaktivitas 60 detik, dan deadline global 15 menit sejak helper terhubung (review L5).

### 7.3a Protokol v2 (FER-472)

Entri `Batch` membawa `modified` (FILETIME last-write dari `$STANDARD_INFORMATION`, u64, 0 = tidak diketahui) setelah
`allocated`, dan `VERSION` menjadi 2 (hello versi 1 ditolak). Dipakai Compare/Sync (`docs/compare-sync.md`); Disk Usage
mengabaikannya. Tidak ada perubahan keamanan lain (pipe, DACL, nonce, token, batas frame).

### 7.4 Alur UX dan fallback

Scan/Rescan memanggil `disk_usage_fast_eligibility(root)` (drive lokal fixed/removable, NTFS, helper terpasang, tanpa
elevasi). Kalau lolos, muncul dialog **"Scan lebih cepat?"** dengan pilihan [Scan biasa] [Scan cepat (admin)] dan
checkbox "Jangan tanya lagi". Checkbox itu hanya mematikan **tawaran**: setelahnya selalu scan biasa, dan tawaran bisa
diaktifkan lagi di Settings. Elevasi tidak pernah diingat; setiap scan cepat melewati UAC. Backend fallback ke walker
(dengan scanId yang sama, dan `notice` berisi alasannya di setiap event) kalau UAC ditolak, volume bukan NTFS lokal,
helper tidak ada, root tidak ketemu, atau terjadi galat protokol/helper. Cancel saat helper berjalan menghasilkan
done + cancelled + `partial`.

Saat helper membaca, event membawa `mftRecordsDone` / `mftRecordsTotal`. UI menampilkan "Membaca MFT… N%" dengan
progress bar, lalu "Menyusun hasil…" ketika `done == total`. Scan biasa tetap tanpa persen (§3.5).

### 7.5 Verifikasi manual UAC (owner)

UAC tidak bisa diotomatisasi dari sesi agent. Langkah untuk owner:

1. Build dengan `npm run tauri build` (helper ikut dibangun sebagai bin kedua). Untuk dev: `cargo build --bin
ferrum-scan-helper`, lalu `npm run tauri dev`. Pastikan `ferrum-scan-helper.exe` ada di samping `ferrum.exe`.
2. Buka Disk usage dengan root `C:\`, lalu Scan. Dialog tawaran harus muncul.
3. Pilih **Scan cepat (admin)**, lalu jawab **Yes** di UAC. Chip `MFT` harus tampil, lalu hasil tree/treemap muncul.
   Catat durasinya.
4. Ulangi, tetapi jawab **No** di UAC. Scan biasa harus berjalan sendiri dengan notice "izin admin ditolak".
5. Pakai root di drive FAT/exFAT atau UNC: dialog tidak muncul dan scan biasa langsung berjalan.
6. Centang "Jangan tanya lagi": scan berikutnya berjalan tanpa dialog. Settings → "Tawarkan scan cepat Disk usage"
   mengembalikan tawaran.
7. Tekan Cancel saat "Membaca MFT…": status harus `cancelled`, dan di Task Manager tidak ada `ferrum-scan-helper.exe`
   yang tertinggal.
8. Di Task Manager → Details, kolom Elevated untuk `ferrum.exe` harus tetap No selama semua langkah.

## 8. Default Instan (indeks) dan cache scan di disk (FER-464)

### 8.1 Aturan

- Root yang tercakup FerrumIndexer (service siap, tidak degraded, indeks drive siap) memakai **Instan (indeks)** sebagai
  default: tombol Scan/Rescan langsung mengambil dari indeks, tanpa dialog dan tanpa UAC. Indeks selalu terbaru (USN),
  jadi **tidak ada cache** untuk root ini. Scan biasa dan scan cepat (admin) tetap bisa dipilih lewat tombol
  "Pilihan scan…" (selalu menampilkan dialog tawaran).
- Preferensi "Jangan tanya lagi" (FER-460/476) hanya mematikan **tawaran**. Jalur Instan tidak menampilkan tawaran,
  jadi preferensi itu tidak membatalkannya; untuk root di luar indeks preferensi itu berlaku seperti sebelumnya
  (scan biasa tanpa tanya). Sync (FER-472) tidak berubah: indeks tetap hanya satu tombol di tawaran.
- Root **di luar indeks** (non-NTFS/exFAT/USB, share jaringan, service tidak terpasang/mati/degraded/indeks sedang
  dibangun) menyimpan hasil scan yang selesai (complete/partial, bukan cancelled) ke cache di disk.

### 8.2 Penyimpanan dan keamanan

- Lokasi: `%LOCALAPPDATA%\<app id>\du-cache` (override `FERRUM_PROFILE_DIR`: `<dir>\data\du-cache`), yaitu profil
  pengguna saat ini dengan ACL bawaan. Scan MFT memuat nama file yang tidak bisa dibaca user biasa (§7.2), jadi tidak
  ada lokasi bersama.
- Nama file = hash SHA-256 dari root yang dinormalisasi (`<32 hex>.fduc`), bukan teks dari user.
- Tulis atomik: file sementara `create_new` di folder yang sama, `sync_all`, lalu `rename`. Folder cache dan file target
  ditolak kalau berupa reparse point (symlink/junction); baca memakai `FILE_FLAG_OPEN_REPARSE_POINT` sehingga link
  tidak pernah diikuti.
- Format `FDUC` v1: header (magic, versi, JSON meta: root, waktu scan, `engine`, stats termasuk coverage asli, jumlah),
  lalu aliran deflate pohon (DFS pre-order, varint, path anak dibangun dari induk) dengan CRC-32 penutup. File rusak
  (terpotong, CRC salah, jumlah/induk mustahil, root tidak cocok dengan nama file) diabaikan, UI jatuh ke scan.
- Batas: 8 root dan 512 MiB total; yang tertua dibuang dulu. Satu entri yang melebihi total tidak disimpan.
- Penyimpanan dijalankan di worker scan SETELAH event `done`, jadi UI tidak menunggu.

### 8.3 Tampilan dan umur

- Saat Disk usage dibuka dan belum ada hasil di layar, root yang punya cache langsung dimuat: chip
  `Stale · hasil 2 hari lalu`, banner dengan tombol **Rescan** dan **Hapus cache**.
- Peringatan muncul kalau umur cache lebih dari ambang Settings (default 24 jam, rentang 1 sampai 8760) atau setelah
  ada perubahan lewat Ferrum (delete/rename/move) pada root tersebut. Marker perubahan disimpan sebagai file
  `<hash>.mut` berisi waktu; cache dianggap berubah kalau waktu marker tidak lebih lama dari waktu scan. Perubahan yang
  terjadi antara akhir scan dan penulisan cache tetap tercatat (daftar perubahan terbaru di memori, 15 menit).
- Settings menampilkan jumlah dan ukuran cache serta tombol "Hapus semua cache".
- Data dari cache hanyalah snapshot biasa: Move/Delete tetap melewati gate revalidasi DSK-06.2 (re-stat sebelum aksi).
