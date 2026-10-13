# Ferrum fase 2 — workflow Total Commander dan arsip ZIP/RAR/7z

**Status:** rencana eksekusi, bukan laporan fitur selesai. **Aplikasi:** `ferrum/` (repo Git tersendiri). **Tanggal:** 28 September 2026.

## 1. Titik awal yang diverifikasi

Ferrum saat ini memakai Tauri 2 + Rust serta Svelte 5 + TypeScript. Repo sudah mempunyai listing direktori, dual pane, tab/history, virtual file list, selection dasar, sort/filter, create/rename/delete, copy/move, antrean serial, command palette, tree sidebar, context menu, inline rename, drag/drop, dark/light/system theme, dan tes. Bukti awal: `src/App.svelte`, `src/lib/actions.ts`, `src/lib/components/FilePane.svelte`, `src-tauri/src/ipc.rs`, `docs/verification-matrix.md`. `src/lib/kind.ts` sudah memberi ikon untuk `.zip`, `.rar`, `.7z`, tetapi belum ada engine arsip atau navigasi ke isi arsip. Audit task F2-00 wajib mengonfirmasi keadaan terbaru sebelum implementasi, karena repo dapat berubah setelah rencana ini ditulis.

Empat screenshot pengguna disalin sebagai referensi fitur:

- [Files menu](reference/total-commander/files-menu.png)
- [Mark menu](reference/total-commander/mark-menu.png)
- [Commands menu](reference/total-commander/commands-menu.png)
- [Show menu](reference/total-commander/show-menu.png)

Tujuan fase ini adalah **kecepatan dan workflow keyboard ala Total Commander** dalam bahasa visual Ferrum. Screenshot adalah daftar kapabilitas dan shortcut rujukan; jangan mengganti desain Ferrum menjadi salinan skin Windows klasik. Setiap aksi baru harus mempunyai state disabled yang benar, pesan error jelas, dan dapat dipanggil melalui menu serta command palette; shortcut hanya bila tidak bentrok dengan yang sudah ada.

## 2. Cara menjalankan dengan Goal Mode

Jalankan **satu F2 task per langkah**. Setelah selesai, cek bukti acceptance sebelum lanjut. Goal yang belum terpenuhi tetap aktif; batas token/waktu bukan tanda selesai. Jika tool berhenti karena permission atau input yang benar-benar tidak tersedia, selesaikan penyebabnya lalu lanjutkan goal yang sama. Jangan berhenti untuk menanyakan pilihan teknis rutin: gunakan keputusan di `docs/decisions.md`, audit kode, dan catat keputusan baru di sana.

Setiap goal harus membaca repo terbaru, menjaga kode lama tetap bekerja, membuat tes bermakna sebelum implementasi perubahan perilaku, menjalankan gate yang relevan, dan melaporkan file/perintah/hasil. Untuk tugas Rust jalankan `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, dan `cargo test`; untuk tugas frontend jalankan `npm run format:check`, `npm run lint`, `npm run check`, `npm run test`, dan `npm run build` sesuai perubahan. Jalankan `npm run coverage`/`cargo llvm-cov` saat logika baru signifikan; target minimal 80% pada logika baru. End-to-end dan smoke desktop wajib untuk alur arsip, destructive operations, dan shortcut lintas pane. Tidak perlu mengulang seluruh suite jika sebuah task hanya mengubah dokumentasi.

### Prioritas bila waktu eksekusi terbatas

1. **P0 arsip berguna:** F2-00 s.d. F2-12. Hasilnya: buka ZIP/7z/RAR sebagai folder virtual, ekstrak aman, buat ZIP/7z, uji integritas, dialog Pack/Unpack.
2. **P1 workflow TC:** F2-13 s.d. F2-24. Hasilnya: menu aksi, selection tingkat lanjut, view modes, search, compare/sync, multi-rename, metadata, hotlist, branch view, checksum. F2-13 dapat dimulai paralel dengan backend arsip; item arsip diaktifkan saat F2-12.
3. **P2 integrasi dan release:** F2-25 s.d. F2-29. Hasilnya: launcher, background jobs, fitur legacy opsional, pengujian lintas OS/performa.

Jangan menjadikan seluruh fase sebagai satu goal. F2-03 sampai F2-12 mengubah kontrak arsip yang sama dan paling aman dikerjakan berurutan. F2-13 menu untuk fitur yang sudah ada dapat dikerjakan paralel dengan F2-01 sampai F2-11; F2-12 baru mengintegrasikan aksi arsip. Task UI lain dapat paralel setelah action contract F2-13 stabil, tetapi jangan menjalankan dua agent yang sama-sama mengedit `actions.ts`, `App.svelte`, atau `ipc.rs` tanpa pembagian file.

## 3. Matriks kapabilitas arsip

| Format                                         | Buka/list    | Ekstrak      | Buat/pack                           | Uji integritas | Password                                                                                         | Multi-volume                                                        |
| ---------------------------------------------- | ------------ | ------------ | ----------------------------------- | -------------- | ------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------- |
| ZIP / ZIP64                                    | Target wajib | Target wajib | Target wajib                        | Target wajib   | Baca terenkripsi bila backend terpilih mendukung; pembuatan terenkripsi task terpisah bila layak | Bukan target awal                                                   |
| 7z (`.7z`; `.7zip` hanya bila signature cocok) | Target wajib | Target wajib | Target wajib                        | Target wajib   | AES bila backend mendukung                                                                       | Tahap lanjut setelah single-volume stabil                           |
| RAR / RAR5                                     | Target wajib | Target wajib | **Tidak dibuat oleh engine bawaan** | Target wajib   | Baca terenkripsi bila backend mendukung                                                          | Target bertahap; deteksi part pertama dan kebutuhan part berikutnya |

RAR creation sengaja tidak dimasukkan sebagai klaim Ferrum. Library UnRAR yang lazim hanya membaca/mengekstrak; ketentuan RARLAB membatasi penggunaan kode UnRAR untuk membuat kompresor RAR. Jika suatu saat diminta, buat task terpisah untuk memakai executable RAR berlisensi yang dipasang pengguna, setelah tinjauan lisensi/distribusi. Jangan diam-diam mengunduh atau membundel executable pihak ketiga.

**Risiko keamanan wajib untuk setiap format:** nama entry tidak boleh keluar dari destination (`..`, absolute path, drive/UNC prefix, NUL, separator ambigu); jangan mengikuti symlink/junction keluar; tolak konflik file/direktori yang berbahaya, path Windows reserved/ADS dan nama yang bentrok hanya pada case; batasi entri, kedalaman, ukuran hasil, dan rasio ekspansi; streaming tanpa menampung file besar di RAM; gunakan staging dan cleanup saat gagal/cancel; jangan log password. Archive tidak tepercaya juga dapat korup, terenkripsi, atau berisi duplicate member. Tes harus memakai temporary directory, termasuk fixture arsip jahat.

**Riset backend yang harus diputuskan di F2-01:** Rust `zip` untuk ZIP; `sevenz_rust2` untuk 7z; pembaca RAR seperti `unrar`/`unrar-rs` dengan telaah lisensi. `libarchive` bisa menjadi alternatif jika kegunaan lintas format mengalahkan biaya native dependency dan bundling. Kunci keputusan setelah spike Windows/Linux/macOS dan capability matrix, bukan hanya berdasarkan nama crate. Lihat [ZIP API](https://docs.rs/zip/latest/zip/read/struct.ZipArchive.html), [7z Rust API](https://docs.rs/sevenz-rust2/latest/sevenz_rust2/), [UnRAR crate](https://docs.rs/unrar/latest/unrar/), [RARLAB license](https://www.rarlab.com/license.htm), dan [Tauri sidecar](https://v2.tauri.app/develop/sidecar/).

## 4. Task dan subtask

### F2-00 — Audit baseline dan lindungi hasil fase 1

**Depends:** tidak ada. **Skill/agen:** `repo-scan`, `planner`, `code-reviewer`. **Prioritas:** P0.

**Subtask:**

1. Baca `README.md`, `docs/decisions.md`, `docs/verification-matrix.md`, `src/lib/actions.ts`, kontrak IPC, dan repo status/commit terakhir; inventaris fitur yang sungguh bekerja vs UI placeholder.
2. Jalankan gate dasar yang tersedia dan catat baseline pass/fail, bukan menyalin angka laporan lama.
3. Simpan matriks gap terhadap screenshot ini dan pilih urutan implementasi akhir pada `docs/phase-2-baseline.md`.
4. Pastikan lokasi fixture/test archive dan ukuran aplikasi baseline tercatat untuk menilai dampak dependency.

**Selesai bila:** gap matrix menunjuk file bukti; baseline perintah/hasil tercatat; tidak ada fitur sudah ada yang direncanakan untuk ditulis ulang.

**Goal siap tempel:**

> Audit repo Ferrum terkini terhadap docs/phase-2-total-commander.md dan empat screenshot referensi. Jalankan gate dasar, identifikasi kemampuan nyata vs placeholder khususnya archive, search, view, compare, dan queue. Tulis docs/phase-2-baseline.md dengan path bukti, status pass/fail/untested, dependency yang ada, dan urutan implementasi; jangan mengubah perilaku aplikasi. Selesai ketika matriks gap serta hasil gate dapat diverifikasi dari repo dan output command.

### F2-01 — Spike backend, format, lisensi, dan fixture arsip

**Depends:** F2-00. **Skill/agen:** `architect`, `docs-lookup`, `security-reviewer`. **Prioritas:** P0.

**Subtask:**

1. Uji kecil kandidat ZIP/7z/RAR untuk list, extract, test, create, password, cancel, progress, dan cross-platform build; ukur binary size serta build time.
2. Catat format/codec yang benar-benar didukung; bedakan ekstensi `.7z` dan `.7zip` nonstandar memakai signature detection.
3. Tinjau lisensi serta kebutuhan native binary/sidecar untuk distribusi Windows/Linux/macOS; catat RAR read-only.
4. Buat corpus kecil legal untuk ZIP/7z/RAR: nested, Unicode, kosong, besar tersimulasikan, korup, encrypted, multipart bila tersedia. Jangan menyimpan data berhak cipta pihak lain sebagai fixture.
5. Putuskan backend di `docs/decisions.md` dan tulis matriks di `docs/archive-support.md`.

**Selesai bila:** proof of concept benar-benar berjalan pada host, batas per format tertulis, keputusan lisensi/distribusi eksplisit, dan fixture dapat direproduksi.

**Goal siap tempel:**

> Evaluasi backend Rust untuk ZIP, 7z, dan RAR di Ferrum melalui spike yang dapat dijalankan. Bandingkan kemampuan list/extract/create/test/password, cross-platform packaging, biaya ukuran/build, dan lisensi. Buat fixture legal yang bisa direproduksi. Rekam keputusan dan kemampuan jujur di docs/archive-support.md serta docs/decisions.md; RAR creation jangan diklaim. Selesai ketika spike host lulus dan pilihan backend didukung hasil command serta sumber primer.

### F2-02 — Kontrak archive provider dan identitas lokasi virtual

**Depends:** F2-01. **Skill/agen:** `architect`, `contract-first`, `rust-reviewer`, `typescript-reviewer`. **Prioritas:** P0.

**Subtask:**

1. Tambah discriminated location type: lokasi disk vs lokasi member arsip (`archivePath`, `memberPath`); jangan memperlakukan member sebagai path OS biasa.
2. Tetapkan `ArchiveEntry`, kemampuan format, metadata, error (`unsupportedCodec`, `passwordRequired`, `badPassword`, `corruptArchive`, `unsafeEntry`, `limitExceeded`), progress, serta kebijakan konflik.
3. Definisikan `ArchiveProvider` read/list/extract/create/test; implementasi format di belakangnya, Tauri command eksplisit, TS guards dan fixture kontrak.
4. Tentukan aturan tab/history, parent di dalam arsip, nested archive (tunda atau kontrak jelas), selection, dan sort/filter tanpa menyalin isi file ke frontend.
5. Uji serialisasi dan race: hasil listing arsip lama tidak boleh mengalahkan tab/path baru.

**Selesai bila:** kontrak Rust/TS konsisten; disk navigation lama tidak rusak; test fixture dan typecheck lulus.

**Goal siap tempel:**

> Tambahkan kontrak archive provider Ferrum sesuai backend terpilih. Modelkan lokasi disk dan lokasi internal archive secara terpisah; definisikan entry, kemampuan per format, error, progress, dan IPC eksplisit dengan fixture Rust/TS. Uji roundtrip, nested path, dan stale request. Jangan ekstrak otomatis hanya untuk listing. Selesai ketika disk workflow lama lulus, kontrak tersinkron, dan semua gate terkait hijau.

### F2-03 — Mesin ekstraksi aman yang dipakai semua format

**Depends:** F2-02. **Skill/agen:** `security-reviewer`, `tdd-guide`, `rust-reviewer`. **Prioritas:** P0.

**Subtask:**

1. Normalisasi nama entry lintas format; tolak absolute/drive/UNC/traversal/NUL/ADS, nama reserved Windows, duplicate collision, dan file-vs-directory collision.
2. Cek containment pada path yang dihasilkan **setelah** resolusi parent/symlink; kebijakan symlink: skip default atau buat hanya jika target tetap di dalam tujuan dan OS mendukung.
3. Tambah quota configurable untuk jumlah entry, total expanded bytes, per-entry bytes, depth, dan ratio warning; UI nanti dapat meminta override eksplisit yang dibatasi.
4. Tulis output lewat staging `.partial`/temporary directory dan commit aman; error/cancel tidak menimpa data pengguna atau meninggalkan file yang dikira lengkap.
5. Uji ZIP slip, 7z traversal, symlink chain, archive bomb sintetis, race destination, permission error, full disk fault, dan cancellation.

**Selesai bila:** semua fixture jahat gagal secara terstruktur dan tidak membuat file di luar destination; fixture normal sukses; cleanup teruji.

**Goal siap tempel:**

> Implementasikan pipeline ekstraksi bersama di Rust yang aman untuk ZIP/7z/RAR: validasi nama member, containment, symlink, collision, batas ekspansi, staging, commit, cancel, dan cleanup. Tulis tes fixture berbahaya lebih dulu dan gunakan temporary directory. Setiap path invalid harus ditolak tanpa menulis di luar destination; kegagalan tidak boleh merusak target lama. Selesai ketika tes keamanan dan gate Rust lulus serta kebijakan tertulis di docs/archive-support.md.

### F2-04 — Buka dan jelajah ZIP/ZIP64 sebagai folder virtual

**Depends:** F2-03. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `frontend-patterns`. **Prioritas:** P0.

**Subtask:**

1. Sniff signature, buka ZIP/ZIP64, tampilkan folder/member tanpa ekstraksi; dukung implicit directory entries, Unicode, empty archive, dan metadata minimum.
2. Masukkan ke dual pane: Enter pada ZIP masuk arsip; Backspace/up kembali ke parent; tab/history tidak kehilangan lokasi luar.
3. List archive besar secara efisien; pagination/streaming bila jumlah entry membuat IPC/DOM berat, tetap virtualized.
4. Tampilkan `archive.zip › folder` di breadcrumb dan badge read-only bila mutasi internal belum ada; navigasi archive satu pane tidak memengaruhi pane lain.

**Selesai bila:** ZIP nyata dapat dijelajah via mouse/keyboard tanpa mengekstrak; history dua pane benar; 10k entry tidak merender 10k DOM row.

**Goal siap tempel:**

> Implementasikan ZIP/ZIP64 listing di Rust dan navigasi isi ZIP sebagai folder virtual di dual pane Ferrum. Enter membuka archive, Up/Back kembali dengan history benar, breadcrumb menunjukkan archive/member, sort/filter/selection/virtual list tetap bekerja. Jangan mengekstrak seluruh ZIP saat browsing. Tulis tes format serta E2E dua pane dan archive 10k entri. Selesai ketika ZIP normal/Unicode/empty/ZIP64 yang didukung dapat dijelajah dan gate lulus.

### F2-05 — Ekstrak ZIP: penuh, sebagian, dan konflik

**Depends:** F2-04. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `security-reviewer`. **Prioritas:** P0.

**Subtask:**

1. Ekstrak seluruh arsip atau selection member ke direktori pane lain/lokasi yang dipilih; preserve struktur path relatif.
2. Integrasikan queue, progress files/bytes, cancel, error per item, conflict policy yang ada (skip/overwrite/rename/older), dan refresh tujuan.
3. Verifikasi CRC saat membaca; tampilkan bad password/unsupported compression secara jelas bila format memakai fitur yang belum didukung.
4. Uji nested folders, kosong, nama Unicode, data besar streaming, gagal tulis, konflik, cancel, dan ZIP slip lewat pipeline F2-03.

**Selesai bila:** extraction tidak freeze UI, output sesuai selection, cancel aman, dan file sumber tetap utuh.

**Goal siap tempel:**

> Tambahkan extract ZIP penuh/selection dari pane archive ke pane filesystem memakai pipeline aman F2-03 dan antrean operasi yang ada. Pertahankan struktur member, terapkan conflict policy, CRC, progress/cancel, dan refresh tujuan. Buat tes integrasi fixture termasuk partial failure dan keyboard flow. Selesai ketika ZIP dapat diekstrak tanpa UI freeze, hasil byte-identical, dan tidak ada path keluar dari destination.

### F2-06 — Buat ZIP/ZIP64 dari selection

**Depends:** F2-05. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `frontend-patterns`. **Prioritas:** P0.

**Subtask:**

1. Pack pilihan file/folder secara streaming ke file sementara; preserve relative paths dan time metadata yang masuk akal; tentukan symlink policy.
2. Opsi awal: format ZIP, nama output, compression level sederhana, include root folder, dan overwrite policy; ZIP64 otomatis untuk batas ukuran/entry.
3. Integrasikan queue/progress/cancel; file tujuan final hanya terlihat sebagai lengkap setelah writer berhasil finalize dan commit.
4. Roundtrip create→list→extract→compare byte dan tes file besar/banyak file kecil/Unicode/conflict.

**Selesai bila:** ZIP dibuat tanpa RAM proporsional isi; cancel tidak meninggalkan ZIP final rusak; hasil dibaca tool ZIP lain.

**Goal siap tempel:**

> Implementasikan Pack to ZIP/ZIP64 dari selection pane disk di Rust, streaming ke temporary output dan commit setelah finalize. Hubungkan conflict policy, progress/cancel, metadata, dan symlink policy. Uji roundtrip terhadap pembaca Ferrum dan tool ZIP lain bila tersedia, termasuk Unicode dan banyak file. Selesai ketika output valid, tidak muncul sebagai selesai sebelum finalize, serta gate lulus.

### F2-07 — Buka dan ekstrak 7z

**Depends:** F2-03, F2-06. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `security-reviewer`. **Prioritas:** P0.

**Subtask:**

1. Implementasikan list tanpa full extract dan deteksi signature 7z; kenali `.7z`, serta `.7zip` hanya bila isi valid.
2. Ekstrak seluruh/selection melalui pipeline F2-03, termasuk solid archive dan codec yang backend dukung; tandai codec unsupported dengan jelas.
3. Integrasikan queue, progress/cancel, conflict policy, memory ceiling dan CRC/data integrity jika tersedia.
4. Tes fixture multi-codec, Unicode, nested, solid, rusak, traversal, quota, dan cancel.

**Selesai bila:** 7z normal dapat dibuka/ekstrak ke pane lain; unsupported codec tidak tampak sebagai arsip kosong; staging aman.

**Goal siap tempel:**

> Tambahkan backend 7z di Ferrum untuk list virtual dan extract seluruh/selection menggunakan engine ekstraksi aman. Deteksi signature, dukung codec yang sudah disetujui di docs/archive-support.md, tampilkan unsupported codec secara eksplisit, dan batasi memori/ekspansi. Hubungkan ke dua pane dan queue. Uji solid, Unicode, corrupt, traversal, quota, cancel, dan roundtrip fixture. Selesai ketika 7z dapat dijelajah/ekstrak tanpa UI freeze dan gate lulus.

### F2-08 — Buat 7z

**Depends:** F2-07. **Skill/agen:** `tdd-guide`, `rust-reviewer`. **Prioritas:** P0.

**Subtask:**

1. Pack selection dengan codec/compression level sederhana yang tersedia dan didokumentasikan; batasi CPU/memori agar UI dan operasi lain tetap responsif.
2. Staging/finalize/commit dan progress/cancel konsisten dengan ZIP; jangan menjanjikan append/update member bila backend hanya mendukung rewrite.
3. Roundtrip pada Ferrum serta 7-Zip CLI bila ada; tes struktur folder, Unicode, file besar, dan cancellation.

**Selesai bila:** file `.7z` valid dan bisa dibuka/ekstrak, proses dibatalkan tanpa mengotori tujuan final.

**Goal siap tempel:**

> Implementasikan Pack to 7z dari selection pane disk memakai backend F2-01. Pilih codec/level yang benar-benar didukung, staging ke file temporary, queue progress/cancel, dan commit setelah finalize. Uji roundtrip list/extract Ferrum dan interoperabilitas 7-Zip jika tersedia; ukur memori pada file besar. Selesai ketika output valid, cancel aman, dan gate lulus.

### F2-09 — Buka dan ekstrak RAR/RAR5 (read-only)

**Depends:** F2-03, F2-08. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `security-reviewer`. **Prioritas:** P0.

**Subtask:**

1. Integrasikan backend RAR yang lulus lisensi F2-01; list tanpa extract; deteksi RAR/RAR5 dan versi/codec tidak didukung.
2. Ekstrak full/selection melalui pipeline aman, queue, progress/cancel, dan conflict policy; bila API backend tidak mendukung cancel granular, dokumentasikan batasnya dan uji.
3. Buka RAR read-only di pane, tanpa tombol Add/Update/Delete internal; Pack dialog menawarkan ZIP/7z saat input berakhir `.rar`.
4. Uji fixture RAR legal untuk single-volume, Unicode, corruption, password-required, traversal, serta proses abort tanpa crash.

**Selesai bila:** RAR normal dapat dibuka/ekstrak, RAR create tidak ditampilkan, dan limitation backend akurat.

**Goal siap tempel:**

> Implementasikan dukungan RAR/RAR5 read-only: list virtual, extract full/selection, test error, progress/cancel semampu backend, dan UI capability yang jujur. Terapkan seluruh kebijakan ekstraksi aman; jangan menambahkan RAR writer atau mengunduh executable. Tes fixture RAR yang legal, corrupt, terenkripsi, dan entry berbahaya. Selesai ketika RAR dapat dibuka/ekstrak pada host tanpa crash dan gate serta audit lisensi lulus.

### F2-10 — Password, multi-volume, dan kompatibilitas arsip

**Depends:** F2-09. **Skill/agen:** `architect`, `tdd-guide`, `security-reviewer`. **Prioritas:** P0.

**Subtask:**

1. Prompt password saat dibutuhkan, mask input, kirim hanya untuk operasi aktif, hapus dari state setelah selesai; jangan simpan di config/log/event/telemetry.
2. Bedakan wrong password vs corrupt archive vs unsupported encryption bila backend bisa membedakan.
3. Deteksi multipart RAR/7z dan minta part pertama/part hilang; jangan ekstrak sebagian lalu klaim sukses. Uji batas backend, dokumentasikan yang belum didukung.
4. Bangun compatibility corpus dari berbagai pembuat archive; tes ZIP AES/ZipCrypto dan 7z AES/RAR encryption sesuai kemampuan yang benar-benar didukung.

**Selesai bila:** password aman di UI/backend, kasus unsupported jelas, multipart yang didukung berfungsi dan yang tidak didukung gagal jujur.

**Goal siap tempel:**

> Lengkapi password handling dan kompatibilitas archive Ferrum sesuai capability backend nyata. Prompt password hanya saat perlu, jangan persist/log, bedakan bad password/corrupt/unsupported, deteksi multipart dan part hilang, serta buat compatibility matrix fixture. Uji tiga format dan E2E dialog. Selesai ketika kasus didukung sukses, kasus tak didukung gagal jelas tanpa output parsial yang diklaim lengkap, dan gate lulus.

### F2-11 — Test Archive(s), verifikasi, dan laporan integritas

**Depends:** F2-10. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `security-reviewer`. **Prioritas:** P0.

**Subtask:**

1. Tambah command Test Archive untuk ZIP/7z/RAR yang membaca/dekompresi dan memverifikasi CRC/checksum format bila tersedia **tanpa menulis hasil ke disk**.
2. Proses selection beberapa archive dalam queue dengan report per archive/per member: passed, failed, skipped/unsupported, password required, dan waktu.
3. Beri cancel/progress dan summary; jangan menganggap berhasil hanya karena central directory/header terbaca.
4. Tes arsip valid, truncated, data bit-flip, wrong password, unsupported codec, dan banyak archive terpilih.

**Selesai bila:** byte korup terdeteksi, laporan tidak menyesatkan, no-extract guarantee teruji.

**Goal siap tempel:**

> Implementasikan Test Archive(s) untuk ZIP, 7z, RAR yang memverifikasi isi stream tanpa mengekstrak ke disk. Tampilkan hasil per member/arsip, progress/cancel, dan status unsupported/password dengan jujur. Uji valid, truncated, bit-flip, wrong password, dan multi-selection. Selesai ketika arsip korup terdeteksi, operasi tidak menulis file tujuan, dan gate lulus.

### F2-12 — Operasi dual-pane melintasi batas arsip + Pack/Unpack UI

**Depends:** F2-11. **Skill/agen:** `architect`, `tdd-guide`, `frontend-a11y`. **Prioritas:** P0.

**Subtask:**

1. Tambah dialog Pack (`Alt+F5`), Unpack Specific Files (`Alt+F9`), Test (`Alt+Shift+F9`) dengan capability per format dan tujuan default pane lain.
2. F5 dari dalam archive ke pane disk = extract selected; F5 dari pane disk ke archive tujuan: tentukan read-only/unsupported dengan pesan jelas sampai update-in-place dirancang; F6 move dari archive memerlukan kebijakan jelas agar tidak menghapus member secara tak sengaja.
3. Drag/drop dari archive ke disk memakai operasi extract; preserve selection/active pane; update breadcrumb/status/archive icon.
4. E2E keyboard: buka ZIP di kiri → pilih member → F5 ke kanan → verifikasi; pack ZIP/7z dari kanan; buka RAR → extract/test; cancel/conflict/password flows.

**Selesai bila:** pengguna dapat browsing, pack, unpack, test dari UI tanpa mengetahui backend; aksi unsupported disabled/berpesan, tidak diam.

**Goal siap tempel:**

> Sambungkan seluruh dukungan ZIP/7z/RAR ke workflow dual-pane Ferrum. Buat Pack Alt+F5, Unpack Alt+F9, Test Alt+Shift+F9; F5 dari pane archive mengekstrak selection ke pane lain, drag/drop mengikuti aturan sama, dan RAR tetap read-only. Jelaskan batas operasi masuk/keluar archive di UI. Tulis E2E keyboard untuk ZIP, 7z, RAR, password/conflict/cancel. Selesai ketika pengguna bisa pack/unpack/test tanpa UI freeze dan semua gate lulus.

### F2-13 — Menu aksi Files/Mark/Commands/Show dan shortcut registry

**Depends:** F2-00 atau audit baseline setara pada branch UI; independen dari F2-01–F2-11. **Skill/agen:** `architect`, `frontend-a11y`, `typescript-reviewer`. **Prioritas:** P1.

**Subtask:**

1. Tambah menu bar empat kelompok sesuai screenshot dengan action registry tunggal. Setiap menu item berbagi id, label, shortcut, `enabled`, dan handler dengan command palette/context menu/keybar.
2. Gunakan kategori dan urutan familiar, tetapi tetap memakai styling Ferrum; menu dapat dibuka mouse maupun keyboard, arrow navigation, Escape, focus restore, ARIA roles.
3. Tampilkan shortcut yang benar per OS; deteksi bentrok dengan shortcut yang ada dan shortcut WebView/OS. Jangan membuat action stub yang pura-pura bekerja.
4. Aksi belum selesai disembunyikan atau diberi disabled + alasan; F2-12 menyalakan aksi arsip setelah E2E lulus.

**Selesai bila:** menu sinkron dengan palette/shortcut, tidak ada shortcut ganda yang memicu dua aksi, fokus keyboard kembali benar.

**Goal siap tempel:**

> Bangun menu Files, Mark, Commands, Show pada Ferrum berdasarkan empat screenshot di docs/reference/total-commander, memakai action registry yang sama dengan palette/shortcut/keybar. Pertahankan visual Ferrum. Implementasikan keyboard navigation, focus restore, disabled state, dan tes konflik shortcut; jangan tampilkan fitur palsu. Selesai ketika seluruh aksi yang sudah ada bekerja dari menu dan aksi yang belum ada jelas tidak aktif, dengan gate frontend lulus.

### F2-14 — Mark: group selection, invert, extension, save/restore

**Depends:** F2-13. **Skill/agen:** `tdd-guide`, `typescript-reviewer`, `frontend-a11y`. **Prioritas:** P1.

**Subtask:**

1. Select/Unselect Group dengan glob/pattern dan preview jumlah match; Select All, Unselect All, Invert, Select All Same Extension. Definisikan dotfiles/no-extension dan case-sensitivity per platform.
2. Save/Restore selection per tab dengan identitas path stabil; Save to File/Load from File memakai format sederhana berversi dengan path validasi.
3. Copy selected names, full paths, dan details ke clipboard; pilih format plain text yang dapat dipaste ke editor/sheet; jangan membocorkan metadata sensitif tanpa aksi pengguna.
4. Semua operasi bekerja dengan virtualized list 100k: manipulasi selection model, bukan DOM rows; hasil sort/filter tidak mengubah identitas pilihan.

**Selesai bila:** menu Mark dari screenshot mempunyai aksi nyata untuk item utama; tes 100k selection tidak mengunci UI; load file tidak memilih path di luar pane tanpa penjelasan.

**Goal siap tempel:**

> Lengkapi Mark menu Ferrum: select/unselect pattern, all/none/invert, same extension, save/restore serta import/export selection, dan copy nama/path/details ke clipboard. Operasikan selection berdasarkan identitas entry, bukan DOM. Tulis tes untuk filter/sort/tab, Unicode, case sensitivity, file selection invalid, dan 100k item. Selesai ketika semua aksi utama Mark berfungsi via menu dan keyboard tanpa UI freeze serta gate lulus.

### F2-15 — Show: mode brief/full/tree/thumbnail, kolom, sorting, refresh

**Depends:** F2-13. **Skill/agen:** `frontend-patterns`, `tdd-guide`, `frontend-a11y`. **Prioritas:** P1.

**Subtask:**

1. Aktifkan Brief dan Full/Details per pane; pertahankan virtualisasi, selection, cursor, dan scroll dengan masing-masing mode.
2. Tambah Tree view dan separate tree bila berguna; thumbnail view dengan lazy loading/thumbnail cache Rust, batas ukuran dan cancel saat scroll; custom columns/view modes melalui konfigurasi bertipe, bukan HTML arbitrary.
3. Quick view panel sebagai shell menunggu F2-17; Comments view hanya jika metadata komentar benar-benar disimpan (F2-27).
4. Sorting Name/Extension/Time/Size/Unsorted/Reverse; sesuaikan shortcut screenshot tanpa menabrak aksi aktif. All files, Programs, Only selected, Custom filter; Reread Source tanpa reset tab/history/selection jika entry masih ada.
5. Toggle Vertical Arrangement dan New Folder Tab bila tidak ada; ukur 10k/100k agar mode baru tetap ringan.

**Selesai bila:** mode dan sort dari Show menu bekerja per pane; state selection stabil; toolbar tidak lagi menampilkan tombol disabled untuk mode yang telah selesai.

**Goal siap tempel:**

> Implementasikan mode Show ala Total Commander pada Ferrum: Brief/Full, tree/thumbnail sesuai batas performa, custom columns, filter preset, sorting name/ext/time/size/unsorted/reverse, reread source, dan vertical arrangement. Per-pane state harus independen. Tulis tes perubahan mode dengan selection/tab/history dan benchmark 10k/100k; aktifkan action menu hanya setelah fitur nyata. Selesai ketika mode utama bekerja dari menu/shortcut dengan gate dan performa yang tercatat.

### F2-16 — Search file/folder Rust, hasil progresif, dan saved search

**Depends:** F2-13. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `frontend-a11y`. **Prioritas:** P1.

**Subtask:**

1. Search current dir/recursive dengan nama glob, ekstensi, ukuran, modified time, dan opsi teks isi file yang dibatasi ukuran/encoding; Rust streaming event/result pages, cancel, symlink loop guard.
2. Search Alt+F7 menampilkan panel hasil yang bisa dinavigasi/dicopy ke pane; Alt+Shift+F7 dapat membuka window/tab terpisah hanya bila Tauri window lifecycle aman.
3. Pilihan literal vs regex jelas; batas jumlah hasil dan backpressure; hasil tetap responsif pada tree besar.
4. E2E navigasi ke hasil, cancel, permission errors, race saat query berubah, dan search archive (opsional setelah kontrak mendukungnya; jelaskan batas).

**Selesai bila:** hasil muncul sebelum traversal selesai, cancel menghentikan I/O, UI tidak freeze, hasil menaut ke path yang benar.

**Goal siap tempel:**

> Implementasikan Search ala Total Commander di Rust dengan hasil progresif, cancel, filter nama/ukuran/waktu dan pencarian isi terbatas. Hubungkan Alt+F7 dan hasil ke dual pane; Alt+Shift+F7 hanya bila window terpisah aman. Uji symlink loop, permission error, Unicode, query berubah cepat, backpressure, dan E2E buka hasil. Selesai ketika hasil terlihat sebelum search besar selesai, cancel bekerja, dan gate lulus.

### F2-17 — Quick View dan preview aman

**Depends:** F2-15. **Skill/agen:** `tdd-guide`, `frontend-a11y`, `rust-reviewer`. **Prioritas:** P1.

**Subtask:**

1. Ctrl+Q membuka quick view di pane lawan atau panel samping sesuai layout; selection/cursor berubah memperbarui preview tanpa membebani UI.
2. Preview awal: text/JSON/Markdown, image, PDF bila backend aman, serta metadata audio/video. Baca file sebagian dengan batas bytes/time; gambar besar memakai thumbnail/downsample.
3. Member archive dibaca streaming ke preview sementara, tidak harus diekstrak permanen; teks HTML/script tidak dieksekusi.
4. Cancel request lama, cleanup URL/blob/temp resources, pesan unsupported/binary/permission jelas.

**Selesai bila:** file besar tidak dimuat seluruhnya ke JS memory; quick view dapat dipakai keyboard only; tes malformed input dan rapid selection lulus.

**Goal siap tempel:**

> Tambahkan Quick View Ctrl+Q dan preview aman untuk text/JSON/Markdown/image serta PDF jika renderer aman, dengan metadata media dasar. Rust membaca terbatas/streaming dan membuat thumbnail; preview tidak mengeksekusi konten tidak tepercaya. Dukung member ZIP/7z/RAR sejauh backend memungkinkan. Uji file besar, malformed, rapid selection, cleanup, dan E2E keyboard. Selesai ketika preview responsif tanpa lonjakan memori atau eksekusi konten serta gate lulus.

### F2-18 — Compare by Content dan Compare Directories

**Depends:** F2-16. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `frontend-patterns`. **Prioritas:** P1.

**Subtask:**

1. Compare by Content untuk dua file terpilih: binary streaming, status equal/different, first mismatch; text diff terbatas untuk file teks dan opsi encoding.
2. Compare Directories Shift+F2 antara root dua pane; hasil same/left-only/right-only/newer/size-different/content-different; tentukan kebijakan waktu dan resolusi timestamp.
3. Mark Newer / Hide Same Files berbasis hasil compare tanpa mengubah file; pilihan dapat dipakai untuk operasi berikutnya.
4. Progres/cancel dan caching hasil yang invalid saat file berubah; archive member dapat dibandingkan read-only jika stream tersedia.

**Selesai bila:** kasus beda isi tetapi ukuran/waktu sama terdeteksi pada mode content; compare direktori tidak menulis data; tes Unicode/mtime skew lulus.

**Goal siap tempel:**

> Implementasikan Compare by Content dan Compare Directories Ferrum untuk dua pane. Bandingkan file binary secara streaming dan text diff terbatas; bandingkan direktori berdasarkan keberadaan/ukuran/waktu dan opsi isi. Tampilkan mark newer/hide same tanpa mutasi file. Tambah progress/cancel dan tes kasus ukuran sama isi beda, timestamp berbeda, Unicode, symlink, archive member, serta E2E. Selesai ketika hasil akurat dan gate lulus.

### F2-19 — Synchronize Directories dengan dry-run wajib

**Depends:** F2-18. **Skill/agen:** `architect`, `tdd-guide`, `security-reviewer`. **Prioritas:** P1.

**Subtask:**

1. Rancang snapshot/plan berbasis compare: kiri→kanan, kanan→kiri, dua arah; classify copy/update/delete/conflict, dengan destination yang eksplisit.
2. Preview setiap rencana sebelum eksekusi; default tidak menghapus dan tidak overwrite tanpa konfirmasi. Keputusan konflik per item + apply all, log hasil/partial failure.
3. Jalankan lewat queue/copy/move yang sudah ada; jangan mengasumsikan mtime saja sebagai bukti isi sama bila pengguna memilih content mode.
4. Tes dry-run no-write, stale plan (file berubah sesudah preview), cancel di tengah, symlink, permission, file conflict, dan dua arah.

**Selesai bila:** dry-run mencerminkan tindakan yang akan dieksekusi, stale plan diblokir/diulang, sumber tidak hilang saat gagal.

**Goal siap tempel:**

> Bangun Synchronize Directories berbasis hasil compare Ferrum. Sediakan mode satu arah/dua arah, preview dry-run terperinci, konflik per item, dan konfirmasi khusus untuk delete/overwrite. Eksekusi melalui queue dengan revalidasi snapshot sebelum menulis. Tulis tes no-write dry-run, stale plan, conflict, cancel, symlink, partial failure, dan E2E. Selesai ketika tindakan nyata sama dengan preview yang disetujui dan kegagalan tidak menghilangkan sumber.

### F2-20 — Multi-Rename Tool dengan preview dan undo terbatas

**Depends:** F2-13. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `frontend-a11y`. **Prioritas:** P1.

**Subtask:**

1. Ctrl+M dialog batch rename dengan token name/ext/counter/date, search/replace, regex opsional, case transform; live preview original→target.
2. Validasi duplicate target, invalid/reserved names, extension handling, case-only rename di Windows, collision dengan file di luar selection, serta swap/cycle melalui temporary names aman.
3. Eksekusi urut/transaction-like dengan report per item; rollback/undo hanya untuk rename sukses yang path-nya belum berubah lagi. Simpan journal sesi, bukan janji undo permanen.
4. Tes 1000 file, Unicode, konflik, two-file swap, cancel, partial failure dan preview-actual equivalence.

**Selesai bila:** semua target preview valid sebelum run, hasil sesuai preview, kegagalan dilaporkan tanpa kehilangan file.

**Goal siap tempel:**

> Implementasikan Multi-Rename Tool Ctrl+M pada Ferrum: aturan nama/ext/counter/date/search-replace/case, preview lengkap, validasi collision dan reserved names, serta eksekusi aman untuk swap/cycle. Beri report/undo sesi yang jujur. Tes Unicode, case-only Windows, duplicate targets, 1000 file, partial failure, dan E2E. Selesai ketika rename actual selalu sesuai preview yang tervalidasi dan gate lulus.

### F2-21 — Properties, attributes, occupied space, volume info

**Depends:** F2-13. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `security-reviewer`. **Prioritas:** P1.

**Subtask:**

1. Alt+Enter Properties: tipe, path, ukuran logis, allocated/occupied bila OS mendukung, waktu, symlink target, permissions, hidden/system/read-only yang relevan.
2. Ctrl+L Calculate Occupied Space untuk selection/direktori recursive dengan progress/cancel, symlink loop guard, dan perbedaan logical vs allocated yang jelas.
3. Change Attributes hanya pada platform/atribut yang aman didukung; perubahan permission/hidden/timestamps lewat dialog preview + error per item. Jangan membuat toggle cross-platform palsu.
4. Volume label dan system info read-only bila tersedia; action disabled/penjelasan jika platform tidak mendukung.
5. Tes large tree, permission denied, symlink, mounted volume, dan UI status.

**Selesai bila:** metadata benar pada host, operasi berat tidak memblokir UI, platform matrix menunjukkan supported/unsupported.

**Goal siap tempel:**

> Tambahkan Properties Alt+Enter, Calculate Occupied Space Ctrl+L, Change Attributes yang didukung OS, serta volume/system info read-only pada Ferrum. Bedakan logical size dan allocated size, jangan ikuti symlink rekursif, beri progress/cancel dan error per item. Buat platform capability matrix serta tes metadata/permission/large tree. Selesai ketika nilai dan dukungan platform jujur, UI responsif, dan gate lulus.

### F2-22 — Directory Hotlist, tree navigation, dan source/target controls

**Depends:** F2-13. **Skill/agen:** `frontend-patterns`, `tdd-guide`, `frontend-a11y`. **Prioritas:** P1.

**Subtask:**

1. Ctrl+D Directory Hotlist: tambah/edit/hapus/reorder lokasi favorit; persistent settings dengan validasi path, fallback saat lokasi hilang.
2. Alt+F10 CD Tree melompat ke direktori lewat tree keyboard; source/target switch Ctrl+U dan target=source; Go Back Alt+Left dan Open Desktop Folder.
3. Tree sidebar yang sudah ada jangan ditulis ulang; integrasikan action yang sama ke menu dan palette.
4. Per-pane tab/history tetap independen saat swap/copy location; tidak memindahkan file hanya karena source/target diswap.

**Selesai bila:** hotlist persist dan navigasi keyboard cepat; swap/path actions tidak merusak selection/history.

**Goal siap tempel:**

> Lengkapi navigasi power-user Ferrum: Directory Hotlist Ctrl+D yang persisten, CD Tree Alt+F10, Go Back Alt+Left, Open Desktop, source-target swap Ctrl+U, dan target=source. Gunakan tree sidebar yang sudah ada. Tulis tes missing path, tab/history/selection setelah swap, keyboard focus, dan E2E. Selesai ketika alur navigasi dapat dipakai tanpa mouse dan gate lulus.

### F2-23 — Branch View / flattened subtree

**Depends:** F2-16, F2-22. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `frontend-patterns`. **Prioritas:** P1.

**Subtask:**

1. Ctrl+B menampilkan semua file dalam subtree sebagai satu daftar virtual; tetap simpan path relatif sehingga nama duplikat tidak ambigu.
2. Lazy/streaming traversal, cancel, symlink loop guard, error per directory, batas item; operasi selection menggunakan path asli.
3. Sorting/filter/search pada hasil branch, breadcrumb kembali ke directory view tanpa kehilangan tab/history.
4. Tes 100k entri, duplicate basename, symlink loops, access denied, cancel, F5 copy selection dari branch.

**Selesai bila:** branch view tidak membuat UI freeze dan operasi terhadap hasil memakai path asli yang benar.

**Goal siap tempel:**

> Implementasikan Branch View Ctrl+B Ferrum: traversal subtree progresif di Rust, daftar virtual dengan relative path, cancel dan guard symlink. Selection/copy/search/sort harus tetap merujuk path asli; kembali ke directory view tidak merusak tab/history. Uji duplicate basename, permission, 100k entries, dan E2E copy dari branch. Selesai ketika hasil besar tetap responsif dan gate lulus.

### F2-24 — Checksums, split/combine, dan verifikasi file

**Depends:** F2-13. **Skill/agen:** `tdd-guide`, `rust-reviewer`, `security-reviewer`. **Prioritas:** P1.

**Subtask:**

1. Create/Verify checksum untuk SHA-256 sebagai default, SHA-1/MD5/CRC32 hanya untuk kompatibilitas verifikasi lama dengan label non-keamanan; dukung manifest `.sha256`/format yang didokumentasikan.
2. Split File dan Combine Files lewat stream dengan part naming deterministik, manifest ukuran/hash, validasi urutan/part hilang sebelum combine; staging + cancel.
3. Integrasikan progress queue, selection, conflict policy, error per file; checksum archive member dapat dijalankan tanpa extract permanen bila stream tersedia.
4. Tes bit-flip, missing part, wrong order, Unicode, file > RAM, cancel, dan roundtrip split→combine.

**Selesai bila:** checksum mismatch terdeteksi; combine gagal tanpa meninggalkan output final yang salah; memory bounded.

**Goal siap tempel:**

> Implementasikan create/verify checksum dan split/combine file di Rust. SHA-256 default; checksum lemah hanya untuk interoperabilitas dengan label jelas. Semua operasi streaming, queued, cancellable, memakai staging dan report. Uji corrupt/missing/out-of-order parts, wrong checksum, file besar, Unicode, dan roundtrip. Selesai ketika mismatch terdeteksi tanpa hasil final rusak dan gate lulus.

### F2-25 — File associations, open external, dan terminal here

**Depends:** F2-13, F2-17. **Skill/agen:** `security-reviewer`, `tdd-guide`, `rust-reviewer`. **Prioritas:** P2.

**Subtask:**

1. Enter pada file menjalankan default OS association dengan API platform yang aman; internal associations hanya untuk preview/editor Ferrum yang terdaftar jelas.
2. Open Terminal Here dan Open command prompt window memakai API terstruktur/arg arrays, cwd dari pane aktif, tanpa shell-string concatenation; prompt jika executable/config tidak ada.
3. Context menu Associate With… membuka OS picker bila tersedia; jangan mengubah system association diam-diam.
4. Tes path dengan spasi, Unicode, quote/metacharacters, UNC, missing executable, file tidak tepercaya; capability Tauri minimal.

**Selesai bila:** open/terminal berjalan pada host tanpa injection dan unsupported OS dilaporkan jelas.

**Goal siap tempel:**

> Tambahkan Open file, Associate With, Internal Associations, dan Open Terminal Here pada Ferrum melalui API OS/Tauri yang aman. Gunakan structured arguments, tidak gabungkan path menjadi shell command, dan jangan ubah association sistem tanpa aksi eksplisit. Uji path spasi/Unicode/metacharacters/UNC serta E2E host. Selesai ketika aksi yang didukung bekerja dan audit injection serta gate lulus.

### F2-26 — Filesystem watcher dan Background Transfer Manager

**Depends:** F2-12, F2-19. **Skill/agen:** `architect`, `tdd-guide`, `rust-reviewer`. **Prioritas:** P2.

**Subtask:**

1. Watch directory aktif per tab/pane dan archive file induknya; debounce, coalesce, cleanup subscription, refresh incremental jika aman, full relist fallback saat event ambigu.
2. Background Transfer Manager memperluas queue yang sudah ada: histori, retry failed, detail per-file, pause/resume hanya jika engine mendukung checkpoint aman; tidak ada kontrol palsu.
3. Persist job hanya bila recovery semantics telah dirancang (file sementara, sumber/tujuan, konflik) dan tidak berisiko mengulang destructive operation.
4. Tes external changes saat listing/search/compare/archive terbuka, watcher storm, device disconnect, cancel/retry race.

**Selesai bila:** UI memperbarui perubahan eksternal tanpa loop refresh; queue state konsisten dan dapat ditelusuri.

**Goal siap tempel:**

> Tambahkan filesystem watcher yang lifecycle-nya mengikuti pane/tab dan perluas OpsPanel menjadi Background Transfer Manager yang menampilkan detail, retry, dan histori. Pause/resume atau persist job hanya bila semantik restart aman; jangan tampilkan tombol palsu. Tes perubahan eksternal, event storm, archive diganti, device disconnect, serta cancel/retry race. Selesai ketika daftar tetap akurat tanpa refresh loop dan gate lulus.

### F2-27 — Fitur menu legacy berprioritas rendah

**Depends:** F2-13, F2-25. **Skill/agen:** `planner`, `tdd-guide`, `security-reviewer`. **Prioritas:** P2, setelah fitur utama stabil.

**Subtask:**

1. `Edit Comment`/Comments view: tentukan penyimpanan sidecar atau metadata platform, konsekuensi rename/move/sync, dan kompatibilitas; implementasi hanya setelah kontrak jelas.
2. Print: serahkan ke OS association print verb bila platform mendukung; jangan membuat renderer cetak khusus tanpa kebutuhan.
3. Decode MIME/UUE/XXE/BinHex dan Encode: ukur kebutuhan nyata, pilih format yang layak dan fixture interoperabilitas; jangan klaim semua hanya dengan satu parser.
4. Custom View Modes/Plugins: simpan preset layout/filter/kolom yang sudah ada; plugin API terpisah setelah kebutuhan nyata.
5. Setiap menu item screenshot mempunyai status implemented/deferred/unsupported dalam matriks, tidak muncul sebagai action fungsional sebelum ada implementasi.

**Selesai bila:** keputusan tiap fitur legacy dicatat; yang dipilih memiliki tes interoperabilitas; menu tidak menyesatkan.

**Goal siap tempel:**

> Audit fitur menu legacy Ferrum terhadap screenshot Total Commander: comments, print, MIME/UUE/XXE/BinHex encode/decode, dan custom view modes. Untuk tiap fitur putuskan implement/defer/unsupported berdasarkan kebutuhan, keamanan, dan kemampuan OS; implementasikan hanya yang dipilih dengan fixture interoperabilitas dan UI yang jujur. Selesai ketika capability matrix lengkap, fitur yang dipilih teruji, dan tidak ada action palsu.

### F2-28 — Verifikasi lintas OS, performa, dan keamanan arsip

**Depends:** F2-12, F2-14 s.d. F2-27 sesuai fitur yang dirilis. **Skill/agen:** `e2e-runner`, `security-reviewer`, `rust-reviewer`, `typescript-reviewer`. **Prioritas:** P2.

**Subtask:**

1. Build/smoke Windows, Linux, macOS secara nyata atau CI runner terpisah; catat pass/fail/untested, bukan klaim generik.
2. Benchmark listing archive 10k/100k members, ZIP/7z pack/extract besar dan ribuan file kecil, RAR extract, search/branch view, waktu startup, memori, CPU, jumlah DOM rows.
3. Audit archive slip, symlink escape, decompression bomb, memory allocation, password/logging, corrupt native decoder crash, dependency advisories, sidecar licensing, command injection.
4. E2E keyboard workflow dua pane dari fixture sampai pack→test→extract→compare; regression tes fase 1.
5. Update README, `docs/archive-support.md`, `docs/verification-matrix.md`, release notes; rekam limitation format dan platform.

**Selesai bila:** release gate yang dijanjikan lulus dengan bukti host, blocker keselamatan ditutup, limitation tersisa jelas.

**Goal siap tempel:**

> Verifikasi fase 2 Ferrum secara menyeluruh. Jalankan gate frontend/Rust, coverage logika baru ≥80%, E2E keyboard pack/test/extract/compare, benchmark archive besar, serta audit keamanan archive/path/password/dependency. Build dan smoke pada OS yang tersedia; tandai OS lain untested. Perbaiki blocker yang dapat diperbaiki dan perbarui dokumentasi matriks hasil. Selesai hanya jika setiap klaim fitur rilis memiliki bukti command atau smoke nyata dan limitation ditulis jujur.

### F2-29 — Release candidate dan backlog berbasis hasil uji

**Depends:** F2-28. **Skill/agen:** `delivery-gate`, `doc-updater`, `planner`. **Prioritas:** P2.

**Subtask:**

1. Kelompokkan kemampuan yang benar-benar siap: Stable, Beta, Unsupported; per format/OS/fitur menu.
2. Susun changelog, upgrade notes, fallback untuk archive unsupported, dan manual keyboard shortcuts ringkas.
3. Buka backlog defect dari hasil uji dengan severity/repro steps, serta rencana fase 3 (network providers, archive update-in-place, plugin API) hanya jika evidence mendukung.
4. Verifikasi artefak installer dan ukuran akhir; tanda tangani rilis/publikasi hanya bila pengguna secara eksplisit memintanya.

**Selesai bila:** release candidate dapat dinilai dari dokumen dan artefak lokal; tidak ada klaim kemampuan yang melebihi tes.

**Goal siap tempel:**

> Siapkan release candidate lokal Ferrum fase 2 dari hasil verifikasi F2-28. Klasifikasikan fitur dan format sebagai Stable/Beta/Unsupported per OS, tulis changelog, shortcut manual, upgrade notes, known issues dengan repro, dan backlog fase berikutnya. Periksa installer lokal serta ukuran final. Jangan mempublikasikan atau mengirim ke pihak luar. Selesai ketika dokumen rilis cocok dengan bukti tes dan artefak lokal dapat ditinjau.

## 5. Pemetaan ke empat screenshot

| Screenshot | Item penting                                                                                       | Task                |
| ---------- | -------------------------------------------------------------------------------------------------- | ------------------- |
| Files      | Pack, Unpack, Test Archive                                                                         | F2-01–F2-12         |
| Files      | Compare by Content, Calculate Occupied Space, Multi-Rename, Properties/Attributes                  | F2-18, F2-21, F2-20 |
| Files      | Split/Combine, Create/Verify Checksums                                                             | F2-24               |
| Files      | Associations, Open/Print, Comments, MIME/UUE/XXE/BinHex                                            | F2-25, F2-27        |
| Mark       | Select/Unselect Group, All/None/Invert/Same Extension, Save/Restore, Clipboard names/paths/details | F2-14               |
| Mark       | Compare Directories, Mark Newer, Hide Same                                                         | F2-18               |
| Commands   | Search, Search separate                                                                            | F2-16               |
| Commands   | Synchronize Dirs                                                                                   | F2-19               |
| Commands   | CD Tree, Hotlist, Go Back, Source↔Target, Target=Source, Desktop                                   | F2-22               |
| Commands   | Branch View                                                                                        | F2-23               |
| Commands   | Background Transfer Manager                                                                        | F2-26               |
| Commands   | Open command prompt, Volume Label, System Info                                                     | F2-25, F2-21        |
| Show       | Brief/Full, Tree, Thumbnail, Custom Columns/View Modes, Only Selected/Programs/All                 | F2-15, F2-27        |
| Show       | Quick View                                                                                         | F2-17               |
| Show       | Sorting, Reversed, Reread, Vertical Arrangement, New Tab                                           | F2-15               |
