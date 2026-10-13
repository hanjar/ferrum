# FerrumIndexer — service Windows (FER-473), tahap S1 (FER-474), S2 (FER-475), S3 (FER-476), FER-480, S4 (FER-477)

Status: **S1 terimplementasi** (skeleton service, install/uninstall MSI + NSIS, IPC terautentikasi, pilihan mode).
**S2 terimplementasi** (indeks MFT + USN journal live per volume NTFS fixed, persistensi terkompresi, pemulihan,
`indexStatus` di IPC v2; lihat §8). **S3 terimplementasi** (FER-476): service SID `Restricted`, query `search` dan
`subtree` di IPC v3 dengan filter AccessCheck di mode Bersama, search dan Disk Usage Ferrum memakai indeks, tool AI
`indexer.status`/`indexer.setMode`; lihat §9. **S4 terimplementasi** (FER-477): IPC v4 dengan `folderSizes`
(hanya High performance) dan `changes` (sync inkremental), kolom ukuran folder opsional; lihat §10. Desain dan
keputusan induk:
`scratch/diskusi-copy-sync-ntfs-2026-10-07.md` bagian 3–5.

## 1. Bentuk

| Bagian                           | Lokasi                                                                                                                   |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| Crate service + protokol + klien | `src-tauri/crates/indexer` (`ferrum-indexer`)                                                                            |
| Binary                           | `ferrum-indexer.exe` (bin kedua paket `ferrum`: `src-tauri/src/bin/ferrum-indexer.rs`), dipasang di samping `ferrum.exe` |
| Klien Ferrum + command Tauri     | `src-tauri/src/indexer_client.rs`: `indexer_status`, `indexer_set_mode`                                                  |
| UI                               | Settings → **Indexer** (`src/lib/components/IndexerSettingsSection.svelte`, IPC `src/lib/ipc/indexer.ts`)                |
| Installer                        | MSI: `src-tauri/windows/indexer-service.wxs`; NSIS: `src-tauri/windows/installer-hooks.nsh`                              |
| Verifikasi manual                | `scripts/indexer-verify.ps1` (read-only) + §6                                                                            |

Perintah binary (parser ketat; argumen lain ditolak sebelum apa pun dibuka):

```text
ferrum-indexer.exe                          dijalankan SCM (service)
ferrum-indexer.exe install [--mode M | --mode-if-new M]     (admin, dipakai installer)
ferrum-indexer.exe uninstall [--purge-data]                 (admin, dipakai installer)
ferrum-indexer.exe stop                                     (admin, sebelum file diganti)
ferrum-indexer.exe has-config                               exit 0 = config valid sudah ada
ferrum-indexer.exe status [--pipe S]
ferrum-indexer.exe set-mode M [--pipe S]                    exit 3 = butuh elevasi, 4 = service mati
ferrum-indexer.exe --console --pipe S [--data-dir DIR] [--mode M]   dev/test, sebagai user biasa
```

## 2. Service dan SCM

- Nama `FerrumIndexer`, tampilan "Ferrum Indexer", `OWN_PROCESS`, **LocalSystem** (dibutuhkan S2 untuk handle volume).
- Start **otomatis (delayed)**. Lifecycle: start, stop, shutdown, **pause/continue** (status `paused` dilaporkan lewat
  IPC; pause menghentikan seluruh pekerjaan indeks, §8.7), interrogate.
- **RequiredPrivileges = `SeChangeNotifyPrivilege` saja.** SCM membuang semua privilege LocalSystem lainnya
  (SeDebug, SeTcb, SeBackup, SeRestore, SeImpersonate, …) dari token service. Impersonation klien pipe di level
  _identification_ tidak butuh privilege. S2 tidak menambah privilege (alasan dan jalur verifikasi: §8.9).
- **Service SID: `Restricted`** (FER-476, `NT SERVICE\FerrumIndexer`). Token service menjadi _write-restricted_:
  setiap hak tulis harus diberikan juga ke salah satu SID pembatas (service SID, Everyone, WRITE RESTRICTED, logon
  SID). Semua yang ditulis service memberi akses ke service SID (folder data dan file-filenya, folder `index`, dan
  DACL pipe). `install` mengemulasi token itu dulu (probe, §8.9); hanya kalau probe gagal service didaftarkan
  `Unrestricted`, dan itu dicatat di `indexer.log` serta di output installer.
- **Query hanya dengan token restricted** (review H1 FER-476). Setiap kali start, service memeriksa token
  prosesnya sendiri: `IsTokenRestricted` dan `TokenRestrictedSids` harus memuat service SID. Tipe SID bisa diganti
  kapan saja dengan `sc sidtype`, jadi hasil probe saat install tidak dipakai di sini. Kalau tidak lolos:
  - `search` dan `subtree` dijawab `degraded`;
  - `status` dan `indexStatus` membawa flag `degraded`;
  - log mencatat `search/subtree disabled (degraded)`;
  - Ferrum memakai scan biasa, dan Settings serta AI `indexer.status` menampilkan "Mode terbatas (degraded)"
    beserta cara memperbaikinya.

  Status, mode, dan indeks tetap berjalan. Mode console (pipe dev, hanya user itu sendiri) menyalakan query secara
  eksplisit.

- **Recovery:** restart setelah 5 detik, lalu 30 detik, lalu berhenti; hitungan reset tiap 24 jam; berlaku juga
  untuk kegagalan non-crash (exit code service-specific).
- Path image di-quote oleh `windows-service` (tidak ada celah unquoted service path). `install` menolak mendaftar
  kalau binary-nya tidak berada di Program Files (build release), karena binary di folder yang bisa ditulis user
  akan berjalan sebagai SYSTEM.

## 3. IPC named pipe

Nama produksi (FER-480): `\\.\pipe\FerrumIndexer`, satu nama untuk SID `restricted` maupun `unrestricted`.

1. **Anti-squatting di sisi klien (FER-480).** Sampai FER-476 pipe berada di
   `\\.\pipe\ProtectedPrefix\Administrators\FerrumIndexer`, yang hanya bisa dibuat oleh anggota Administrators.
   Di instalasi nyata, service dengan SID `restricted` ternyata **tidak bisa** membuat pipe di sana
   (`Access is denied`, apa pun DACL pipe-nya), sehingga query mati (lihat §8.11). Karena itu pipe dipindah ke akar
   NPFS, dan pertahanan squatting pindah ke klien. Nama di akar NPFS bisa dibuat siapa pun. Karena itu klien
   (Ferrum dan `ferrum-indexer.exe status|set-mode|…`) **memverifikasi server sebelum mengirim byte pertama**
   (`pipe::verify_server`, dipanggil dari `pipe::open_client`):
   1. owner pipe harus SYSTEM atau Administrators (butir 5);
   2. `GetNamedPipeServerProcessId` harus sama dengan PID service FerrumIndexer menurut SCM
      (`QueryServiceStatusEx`; state Running, Paused, PausePending, atau ContinuePending);
   3. image proses itu (`NtQuerySystemInformation(SystemProcessIdInformation)`, bisa dibaca user standar tanpa
      membuka proses) harus **file yang sama** (serial volume + file index) dengan image service terdaftar
      (`QueryServiceConfigW`, path ber-quote tanpa argumen, di Program Files pada build release);
   4. kalau klien boleh membuka token proses server (klien elevated), token itu harus memuat
      `NT SERVICE\FerrumIndexer` sebagai grup enabled. User standar tidak boleh membuka proses service SYSTEM
      (`OpenProcess` = ERROR_ACCESS_DENIED, diukur dengan `scripts\indexer-pipe-probe.ps1 -Client`). Bagi user
      standar cek ini dilewati, sedangkan error lain membuat verifikasi gagal.

   Kalau satu cek gagal, klien menutup handle tanpa mengirim apa pun, lalu Ferrum memakai scan biasa. Alasannya
   dibedakan (review F1):
   - **owner atau PID tidak cocok** (atau service tidak berjalan padahal pipe ada) → `ClientError::Squatted` dengan
     PID server. Settings → Indexer dan AI `indexer.status` menampilkan "Nama pipe FerrumIndexer dipakai proses lain
     (PID n) — indeks tidak dipakai, scan biasa aktif". CLI: `error: pipe name taken by another process (pid n): …`.
   - **image, token, atau SCM gagal** → `ClientError::Untrusted`. Settings menampilkan "Tidak tepercaya…", dan CLI
     `error: untrusted service pipe: …`. Tes: `pipe::tests::verify_*`, `rejected_server_receives_nothing` (server palsu
     tidak menerima satu byte pun), `service_endpoint_refuses_a_squatter`, dan
     `console_ipc::client_verifies_the_server_identity`.
     Batasnya: membuka pipe tetap memberi tahu server PID klien (`GetNamedPipeClientProcessId`). Server tidak bisa
     impersonate sebelum membaca data, sehingga squatter tidak mendapat token, SID, atau query.

2. **`FILE_FLAG_FIRST_PIPE_INSTANCE`** pada instance pertama. Kalau nama sudah dipegang proses lain,
   **FER-487:** kalau gagal dengan `ERROR_ACCESS_DENIED`, service langsung memakai nama cadangan
   (§3.2). Hanya kalau nama cadangan pun gagal, service **mencoba lagi tiap 2 detik** sampai nama bebas atau service
   dihentikan. Tiap ±5 menit service menulis satu
   baris log yang menyebut pemegang nama: PID server, owner pipe, dan image (`pipe::describe_pipe_holder`). Untuk
   itu service membuka pipe squatter sebagai klien (identification, hak klien saja), lalu menutupnya **tanpa
   mengirim apa pun**. Contoh:
   `cannot create pipe \\.\pipe\FerrumIndexer (…); held by pid 1234, owner S-1-5-21-…, image \Device\…\x.exe; clients refuse it, retrying`.
   Flag `pipe_squatted` di `status`/`indexStatus` **tidak** dibuat: selama nama dipegang squatter, tidak ada klien
   yang bisa mencapai service, jadi flag itu tidak pernah terbaca. Penggantinya adalah log dan PID squatter di sisi
   klien. Service tidak lagi
   keluar dengan error, karena itu akan menghabiskan aksi recovery SCM. Instance berikutnya selalu dibuat
   **sebelum** instance yang terhubung diserahkan ke worker, jadi selama service berjalan nama tidak pernah tanpa
   instance, dan DACL di bawah mencegah klien menambah instance sendiri. Maksimal 10 instance.
3. **DACL** (`pipe::pipe_sddl`): `O:SY D:P(D;;GA;;;NU)(D;;GA;;;AN)(A;;GA;;;SY)(A;;GA;;;<service SID>)(A;;0x120183;;;IU) S:(ML;;NWNR;;;ME)`.
   Review F2: owner **SYSTEM eksplisit** untuk service, supaya owner tidak bergantung pada TokenOwner token service.
   Probe install memakai DACL yang sama tanpa owner (`pipe::service_dacl_sddl` + ACE pembuat), karena admin tidak bisa
   memasang `O:SY`. Kalau `O:SY` suatu saat ditolak (`ERROR_INVALID_OWNER`), service sekali beralih ke DACL tanpa
   owner dan mencatatnya di log. Klien tetap hanya menerima owner SYSTEM atau Administrators.
   - Hanya **INTERACTIVE** (konsol/RDP) yang boleh membuka; logon network/service/batch tidak.
   - Hak klien `0x120183` = read/write data, read/write attributes, READ_CONTROL, SYNCHRONIZE. **Tanpa
     `FILE_CREATE_PIPE_INSTANCE`** (bagian dari `FILE_GENERIC_WRITE`), jadi klien tidak bisa menambah instance pipe
     sendiri. Dicek saat kompilasi dan di tes `interactive_clients_cannot_add_instances`.
   - Label medium no-read-up/no-write-up: proses low-IL (sandbox browser) tidak bisa membuka.
   - `PIPE_REJECT_REMOTE_CLIENTS`.
4. **Autentikasi klien lewat token, sebelum apa pun dijawab.** Klien mengirim `Hello` kosong (magic `FRIX` +
   versi), karena server harus membaca dulu sebelum bisa impersonate. Server: `ImpersonateNamedPipeClient` →
   `OpenThreadToken` → `RevertToSelf` (gagal = `abort`), lalu membaca SID user, integrity, elevasi, keanggotaan
   Administrators **yang enabled** (`CheckTokenMembership`), keanggotaan INTERACTIVE, token restricted, dan
   AppContainer. Ditolak kalau tidak interaktif, integrity < medium, restricted, atau AppContainer
   (`policy::accept_client`). Klien membuka pipe dengan `SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`,
   sehingga service hanya bisa mengidentifikasi, tidak pernah bertindak sebagai user.
5. **Verifikasi owner oleh klien.** Sebelum mengirim byte pertama, klien membaca **owner** pipe
   (`GetSecurityInfo`, butuh READ_CONTROL) dan hanya menerima SYSTEM atau Administrators. User standar tidak bisa
   menjadikan SYSTEM atau Administrators sebagai owner objeknya. Untuk pipe service, cek PID/image/token di butir 1
   menyusul. Log service mencatat owner yang dipakai: `listening on \\.\pipe\FerrumIndexer (owner S-1-5-…)`.
6. **Frame berbatas** (`proto.rs`): `len u32 + kind u8 + payload`. Request maks 256 byte, response maks 4 KiB
   (S1; sejak v3 2 KiB / 1 MiB, sejak v4 request 16 KiB, §10),
   teks maks 200 byte UTF-8 tanpa karakter kontrol. Enum dicek, trailing byte ditolak, panjang dicek sebelum
   alokasi. Fuzz deterministik 20.000 kasus di kedua decoder. Frame rusak → `bad_request` lalu koneksi ditutup.
7. **Batas laju dan waktu** (`policy::limits`): token bucket 20 burst / 10 per detik per koneksi; lebih dari 20
   penolakan → koneksi diputus. Maks 8 koneksi bersamaan (lebih → `busy`). Hello harus datang dalam 5 detik,
   idle 30 detik, write 5 detik, umur koneksi maks 10 menit (membatasi slow-loris).
8. **Permukaan perintah:** `ping`, `status`, `version`, `getMode`, `setMode` (S1), `indexStatus` (S2), `search`
   dan `subtree` (S3, §9), `folderSizes` dan `changes` (S4, §10). Tidak ada perintah tulis file, eksekusi, atau
   baca isi file.
9. **DACL pipe dan SID Restricted (FER-476):** DACL juga memberi `GA` ke service SID. Tanpa ACE itu token
   write-restricted tidak bisa menambah instance pipe (`FILE_CREATE_PIPE_INSTANCE` termasuk hak tulis), dan accept
   loop berhenti setelah klien pertama. Ini dibuktikan tes `restricted_token_can_add_instances_only_with_service_ace`.
   Klien tidak pernah memegang service SID.

### 3.1 Aturan `setMode`

| Perubahan                                       | Siapa                                                                      |
| ----------------------------------------------- | -------------------------------------------------------------------------- |
| ke `shared` (lebih privat) atau tanpa perubahan | klien interaktif terautentikasi mana pun                                   |
| `shared` → `high_performance` (kurang privat)   | hanya **administrator elevated** (token elevated + Administrators enabled) |

Di PC bersama, user standar tidak boleh membuat nama file user lain terlihat. Ferrum berjalan non-elevated, jadi
untuk menaikkan ke High performance Ferrum menjalankan `ferrum-indexer.exe set-mode high_performance` lewat UAC
(binary di samping `ferrum.exe`, build release hanya dari Program Files, seperti aturan helper FER-460). Proses
elevated itu hanya klien pipe biasa, jadi aturan di service tetap satu-satunya penentu. Setiap perubahan
dipersist **dulu** ke config; mode di memori baru berubah kalau tulis berhasil. Perubahan dicatat di log (SID +
elevated).

Di sisi Ferrum, **kedua arah wajib dialog konfirmasi** yang menjelaskan konsekuensi privasi. Untuk High
performance tombolnya baru aktif setelah centang "Saya mengerti, PC ini hanya dipakai oleh saya". Command
`indexer_set_mode` menolak tanpa `confirmed: true`. Kapabilitas AI `indexer.setMode` (S4) wajib melewati dialog
yang sama.

## 3.2 FER-487: nama cadangan melawan squatting

**Masalah.** Nama tetap `\.\pipe\FerrumIndexer` di akar NPFS bisa diambil user standar lebih dulu (squatting). Klien
tidak membocorkan data karena memverifikasi server (§3 butir 1), tetapi indeks tidak bisa dipakai (DoS).

**Service (`server::create_first`, `announce.rs`).**

1. Saat start, value `PipeName` lama dihapus (`announce::clear`).
2. Instance pertama dibuat di nama tetap dengan `FILE_FLAG_FIRST_PIPE_INSTANCE` dan DACL/owner seperti §3 butir 3.
3. Kalau gagal dengan `ERROR_ACCESS_DENIED` (nama sudah dipegang), service membuat `FerrumIndexer-<32 hex>` dari
   `BCryptGenRandom`, dengan DACL yang sama dan `FIRST_PIPE_INSTANCE`, lalu melayani di nama itu. Selama run ini nama
   tetap tidak dicoba lagi; service kembali ke nama tetap hanya setelah restart.
4. **Pipe dibuat dulu, baru nama diumumkan.** Setelah instance pertama ada, service menulis nama itu ke
   `HKLM\SYSTEM\CurrentControlSet\Services\FerrumIndexer\Parameters`, value `PipeName` (REG_SZ). Jadi nama yang
   diumumkan selalu sudah dipegang service.
5. Kalau nama tetap bebas, tidak ada yang diumumkan.

**Kunci pengumuman.** Installer membuat `Parameters` dengan SDDL
`O:BAD:P(A;CI;KA;;;SY)(A;CI;KA;;;BA)(A;CI;KW;;;<service SID>)(A;CI;KR;;;BU)`: owner Administrators, SYSTEM dan
Administrators penuh, service SID hanya `KEY_WRITE` (satu-satunya hak tulis untuk token write-restricted, FER-476),
Users hanya baca, tanpa mewarisi. Install juga menulis dan menghapus `PipeNameProbe` dengan token Restricted. Kalau
probe gagal, install **berhenti** dan SID type tidak dilonggarkan.

**Klien (`pipe::open_client` lalu `open_service`).**

1. Buka nama tetap dan verifikasi server seperti §3 butir 1.
2. Kalau hasilnya Squatted, Untrusted, atau NotRunning (nama tetap tidak ada, misalnya setelah squatter keluar
   sementara service memakai nama cadangan), baca `PipeName`. Ini hanya dilakukan kalau SCM melaporkan service
   berjalan; tanpa itu, pipe yang ada tetap dianggap squatter.
3. Nilai dibaca **hanya kalau** kunci `Services\FerrumIndexer` dan `Parameters` lulus `announce::key_is_trusted`:
   owner SYSTEM atau Administrators, tidak ada DACL NULL, dan tidak ada ACE ALLOW yang memberi hak tulis (data, subkey,
   link, DELETE, WRITE_DAC, WRITE_OWNER, GENERIC_WRITE/ALL) kepada SID selain SYSTEM, Administrators, dan service SID
   (termasuk Users, Authenticated Users, Everyone). Kunci induk ikut dicek karena user yang bisa membuat subkey di
   `Services\FerrumIndexer` bisa mengganti seluruh `Parameters`.
4. Nilai harus **persis** `FerrumIndexer-[0-9a-f]{32}`. Selain itu nilai diabaikan.
5. Nama itu dibuka dengan pemeriksaan owner dan `verify_server` yang sama persis (PID dari SCM, image, token).
   Nama dari registry hanya petunjuk; kepercayaan tetap datang dari verifikasi server.

**Batas.**

- Squatter di nama tetap tidak lagi mematikan indeks. Squatter yang dijalankan administrator tetap di luar model
  ancaman (sama seperti §3 butir 1).
- Ketergantungan: ACL default `Services\FerrumIndexer` dari SCM harus lolos `key_is_trusted`. Ini dicek dua
  kali: tes `real_service_key_sddl_is_trusted` memakai SDDL nyata mesin tes (ACE CREATOR OWNER-nya inherit-only,
  jadi dilewati), dan `live_service_key_is_trusted_when_installed` membaca kunci hidup (read-only). Cek yang sama ada
  di `scripts\indexer-verify.ps1`. Kalau kunci induk gagal, klien gagal tertutup dan memakai scan biasa.
- Install lama tanpa key `Parameters` tidak mengumumkan apa pun; klien hanya bisa lewat nama tetap, dan log service
  mencatat kegagalan pengumuman.

**Tes.** `announce::tests` (validasi nama, aturan kunci, registry mock, SDDL), `pipe::tests::fallback_*`,
`pipe::tests::no_announcement_keeps_the_primary_error`, `pipe::tests::no_fallback_is_tried_when_the_service_is_not_running`,
`pipe::tests::first_instance_on_a_taken_name_is_access_denied`, dan `server::tests::service_falls_back_when_the_fixed_name_is_squatted`
(yang terakhir hanya jalan kalau nama tetap bebas di mesin tes). Yang **tidak** bisa diuji tanpa admin dan install:
ACL key nyata, penulisan `PipeName` oleh token Restricted, dan jalur klien end-to-end dengan service nyata.

## 4. Config dan data: `C:\ProgramData\Ferrum`

- `indexer.conf` (format `key=value`, bukan JSON, supaya parser di proses SYSTEM tetap kecil):
  `version=1`, `mode=high_performance|shared`. Maks 4 KiB. Key tidak dikenal, duplikat, versi lain, atau mode tidak
  valid → **fallback `shared`** (aman untuk privasi) + catatan di log.
- Direktori: owner `BUILTIN\Administrators`, DACL **terproteksi** `SYSTEM:F` + `Administrators:F` (OICI), tanpa
  warisan dari ProgramData (yang mengizinkan Users membuat folder).
- **Folder atau file tanam (review H1).** Siapa pun bisa membuat `C:\ProgramData\Ferrum` lebih dulu (atau junction
  bernama itu), lalu menaruh `indexer.conf` berisi `mode=high_performance` atau link `indexer.log` ke file sistem.
  Aturannya (`store.rs`):
  - Folder dibuat **atomik** dengan `CreateDirectoryW` + security descriptor final (owner Administrators, DACL
    terproteksi SYSTEM + Administrators). Tidak pernah dibuat dulu lalu diamankan belakangan.
  - Folder yang sudah ada hanya dipakai kalau berupa direktori biasa (bukan reparse point) dengan owner
    SYSTEM/Administrators, dicek lewat handle `FILE_FLAG_OPEN_REPARSE_POINT`. Kalau tidak lolos, `install`
    me-rename-nya ke `Ferrum.untrusted-<waktu>` **lewat handle yang sudah dicek** (`SetFileInformationByHandle`;
    tidak dipakai, tidak dihapus), lalu membuat folder baru secara atomik. Kalau di fase kedua nama itu sudah ada
    lagi, `install` **gagal**: folder yang tidak dibuatnya sendiri tidak pernah diadopsi. Sisa risiko yang diterima:
    user yang menahan handle folder tanam tanpa `FILE_SHARE_DELETE` bisa menggagalkan rename, sehingga service tidak
    terpasang (DoS pemasangan). Service sendiri tidak pernah me-rename; ia berjalan dengan `shared` di memori tanpa
    persistensi.
  - Selama dipakai, handle folder ditahan dengan akses data dan **tanpa `FILE_SHARE_DELETE`**, sehingga folder yang
    sudah diverifikasi tidak bisa di-rename atau diganti (diuji).
  - Setiap file anak (`indexer.conf`, `indexer.conf.tmp`, `indexer.log`, `indexer.log.1`) dibuka dengan
    `FILE_FLAG_OPEN_REPARSE_POINT` lalu dicek **setelah dibuka**: bukan direktori, bukan reparse point, jumlah link 1,
    owner SYSTEM/Administrators. Yang tidak lolos dihapus lewat handle-nya sendiri (yang terhapus link-nya, bukan
    targetnya). Config yang tidak tepercaya diperlakukan sebagai `shared`.
  - Semua baca/tulis/rename lewat handle terverifikasi. Config ditulis atomik: `.tmp` baru (`CREATE_NEW`, SD
    eksplisit) → flush → rename-by-handle dengan replace (mengganti entri direktori, tidak menulis menembus link).
    Log di-append lewat handle terverifikasi; rotasinya juga rename-by-handle.
- Log: `indexer.log` (rotasi 1 MiB ke `.log.1`), hanya diagnostik singkat (mode, SID, kode error), tidak pernah
  nama file atau isi request.
- Ferrum tidak membaca folder ini; semua status lewat pipe.

## 5. Installer

Keduanya per-machine (UAC sekali). Registrasi dilakukan `ferrum-indexer.exe` sendiri, jadi MSI dan NSIS memakai satu
jalur kode yang teruji. Kegagalan langkah service **tidak** menggagalkan instalasi Ferrum (Ferrum tetap jalan tanpa
service; Settings → Indexer menampilkan "Tidak terpasang").

| Aspek                  | MSI (WiX fragment)                                                                                                                                                        | NSIS (hooks)                                                                                                   |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- |
| Pilihan mode           | Dialog radio setelah halaman folder ("PC dipakai sendiri: High performance (disarankan)" / "PC dipakai bersama"). Dipakai hanya kalau config belum ada (`--mode-if-new`). | MessageBox Ya/Tidak dengan teks yang sama, hanya kalau `has-config` ≠ 0.                                       |
| Silent                 | `msiexec /i Ferrum.msi /qn FERRUMINDEXERMODE=shared` (tanpa properti → `shared` untuk instalasi baru)                                                                     | `/S` → `shared`; `/INDEXERMODE=high_performance\|shared` juga hanya berlaku kalau config belum ada (review I1) |
| Stop / hapus service   | `ServiceControl` (StopServices / DeleteServices) milik Windows Installer                                                                                                  | `net.exe stop` + `sc.exe delete` dari System32                                                                 |
| Eksekusi exe           | `WixQuietExec64` (tanpa jendela), deferred, LocalSystem, hanya kalau `INSTALLDIR` di bawah Program Files                                                                  | `nsExec`, hanya kalau `$INSTDIR` di bawah Program Files dan tanpa `\..`                                        |
| Upgrade / reinstall    | Service di-stop sebelum file diganti; produk lama menghapus service-nya (data tetap), lalu `install` mendaftarkan lagi (mode lama dipertahankan)                          | Pre-install `net stop`; post-install `install` (mode lama dipertahankan)                                       |
| Repair (`msiexec /fa`) | StopServices menghentikan service, lalu `install` (tanpa mode dari command line) mendaftarkan dan menjalankannya lagi                                                     | —                                                                                                              |
| Uninstall              | Service di-stop dan dihapus. Data tetap, kecuali `FERRUMINDEXERPURGE=1`                                                                                                   | Service di-stop dan dihapus. Centang "Delete the application data" juga menjalankan `--purge-data`             |
| Rollback               | `uninstall` hanya untuk instalasi baru, bukan upgrade (review L3)                                                                                                         | —                                                                                                              |

**Folder instalasi di luar Program Files (review M1).** Folder seperti `D:\Apps\Ferrum` bisa ditulis user biasa, dan apa
pun yang ditaruh di sana akan berjalan sebagai admin/SYSTEM. Karena itu kedua installer **tidak** mengeksekusi
`ferrum-indexer.exe` dan tidak memasang service. Settings → Indexer menjelaskan alasannya (`problem: location`).

**Repair oleh user standar (review H2).** Produk per-machine boleh di-repair oleh user standar
(`msiexec /fa <msi cache>`). Itu perilaku standar Windows Installer: repair menghentikan lalu menjalankan ulang service
(`ServiceControl` + `install`). Repair tidak memunculkan jendela console SYSTEM (WixQuietExec64), tidak mengeksekusi apa
pun dari folder yang bisa ditulis user, dan tidak menerapkan mode dari command line. Sisanya hanya service berhenti
sesaat (DoS ringan, diterima).

`--purge-data` hanya menghapus file milik service (`indexer.conf`, `.tmp`, log) dan direktori bila kosong, dan
menolak kalau direktori tidak tepercaya.

Perubahan `tauri.conf.json` dibuat minimal: `bundle.windows.nsis.installerHooks` dan `bundle.windows.wix`
(`fragmentPaths`, `componentGroupRefs`).

## 6. Verifikasi manual (owner, butuh admin)

Sesi agent tidak memasang service. Langkah untuk owner:

1. Build: `npm run tauri build`. Hasil: `src-tauri\target\release\bundle\msi\Ferrum_0.1.0_x64_en-US.msi` dan
   `...\bundle\nsis\Ferrum_0.1.0_x64-setup.exe`. Pakai **salah satu** saja.
2. **MSI:** jalankan installer. Setelah halaman folder harus muncul dialog "Ferrum Indexer — Bagaimana PC ini
   dipakai?" dengan pilihan High performance (terpilih) dan Bersama. Pilih salah satu, selesaikan.
   **NSIS:** jalankan setup. Menjelang selesai muncul MessageBox "Apakah PC ini hanya dipakai oleh Anda sendiri?".
3. PowerShell **elevated**: `powershell -ExecutionPolicy Bypass -File scripts\indexer-verify.ps1`. Semua baris
   harus PASS (service terdaftar, LocalSystem, delayed auto start, path ber-quote di Program Files, SID
   UNRESTRICTED, privilege hanya SeChangeNotify, recovery RESTART, RUNNING, ACL ProgramData\Ferrum hanya SYSTEM +
   Administrators, owner Administrators, `indexer.conf` berisi mode yang dipilih, status lewat pipe).
4. Buka Ferrum (biasa, non-admin) → Settings → **Indexer**: Terpasang, Berjalan, versi 0.1.0, mode sesuai pilihan.
5. Kalau mode = Bersama: klik **Ganti ke High performance…** → dialog harus menjelaskan bahwa file akun lain ikut
   terindeks. Tombol "Ganti mode" nonaktif sampai checkbox dicentang. Centang → Ganti mode → **UAC** muncul
   (`ferrum-indexer.exe`). Jawab **No** → pesan "Izin administrator ditolak", mode tetap. Ulangi, jawab **Yes** →
   mode menjadi High performance.
6. **Ganti ke Bersama…** → dialog → Ganti mode: berubah tanpa UAC.
7. PowerShell **non-admin**: `& "C:\Program Files\Ferrum\ferrum-indexer.exe" set-mode high_performance` → exit 3
   ("needs_elevation"). `... status` → `state=running mode=…`.
8. `sc.exe pause FerrumIndexer` (admin) → Settings → Periksa lagi: status "Dijeda". `sc.exe continue FerrumIndexer`.
9. `sc.exe stop FerrumIndexer` (admin) → Settings → Periksa lagi: "Berhenti" + teks fallback; fitur Ferrum lain
   tetap normal. `sc.exe start FerrumIndexer` → kembali Berjalan.
10. Recovery: Task Manager → Details → akhiri `ferrum-indexer.exe` → dalam ±5 detik service hidup lagi
    (`sc.exe query FerrumIndexer`).
11. Squatting (opsional, FER-480): saat service **berhenti**, dari PowerShell non-admin:
    `$s = [System.IO.Pipes.NamedPipeServerStream]::new('FerrumIndexer')` (berhasil, karena nama di akar NPFS bebas
    dibuat). Lalu `sc.exe start FerrumIndexer` (admin) dan `& "C:\Program Files\Ferrum\ferrum-indexer.exe" status`
    → harus `error: pipe name taken by another process (pid <PID PowerShell itu>): …` (exit 1), dan Settings →
    Indexer → "Nama pipe FerrumIndexer dipakai proses lain (PID …)". `indexer.log` berisi
    `cannot create pipe \\.\pipe\FerrumIndexer (...); held by pid <PID>, owner <SID user>, image …powershell.exe; … retrying`. `$s.Dispose()` → dalam ±2 detik log
    `pipe … created after N retries`, dan `status` normal lagi.
12. Upgrade: pasang installer yang sama lagi → service di-stop, file diganti tanpa "file in use", mode lama
    tetap (tidak ditanya lagi di NSIS).
13. Uninstall tanpa hapus data → `sc.exe query FerrumIndexer` = service tidak ada; `C:\ProgramData\Ferrum\indexer.conf`
    masih ada. Uninstall dengan purge (NSIS: centang "Delete the application data"; MSI:
    `msiexec /x Ferrum.msi FERRUMINDEXERPURGE=1`) → `indexer.conf` dan log hilang.
14. **Repair sebagai user standar (H2):** login sebagai user non-admin, cari MSI cache Ferrum
    (`Get-ChildItem C:\Windows\Installer\*.msi`, lihat Subject di Properties), lalu
    `msiexec /fa "C:\Windows\Installer\<file>.msi"`. Selama repair **tidak boleh** muncul jendela console apa pun.
    Setelahnya `sc.exe query FerrumIndexer` = RUNNING, dan mode tetap.
15. **Folder di luar Program Files (M1):** uninstall, lalu pasang MSI ke `D:\FerrumTest\` (Change folder) atau NSIS ke
    `D:\FerrumTest`. `sc.exe query FerrumIndexer` → service **tidak ada**. Settings → Indexer menampilkan "tidak
    dipasang karena Ferrum terpasang di luar Program Files". Uninstall bersih.
16. **Folder tanam (H1):** uninstall dengan purge (`C:\ProgramData\Ferrum` hilang). Sebagai user non-admin, buat
    `C:\ProgramData\Ferrum\indexer.conf` berisi `version=1` dan `mode=high_performance`. Pasang ulang, pilih
    **Bersama**. Hasilnya: ada `C:\ProgramData\Ferrum.untrusted-<angka>` (folder tanam), `C:\ProgramData\Ferrum`
    baru dengan owner Administrators, dan mode = shared.
17. Event Viewer → Windows Logs → System: tidak ada error Service Control Manager untuk FerrumIndexer selama
    langkah di atas (selain stop/kill yang disengaja).

Kalau langkah 3 gagal di "service RUNNING" dengan error privilege: tambahkan privilege yang disebut di Event Viewer
ke `REQUIRED_PRIVILEGES` (`crates/indexer/src/service.rs`), lalu laporkan di FER-474.

### 6.1 FER-487: kegagalan install setelah service terdaftar

Langkah `install` yang memasang kunci `PipeName` (`create_parameters_key`) dan probe token Restricted
(`probe_pipe_name_write`) dijalankan setelah service terdaftar. Kalau salah satunya gagal, `install`:

- berhenti dengan pesan `install stopped: <sebab>; <tindakan>`, dan exit code bukan 0;
- service yang **dibuat** oleh pemanggilan itu dihapus lagi (instalasi baru tidak meninggalkan apa pun);
- service yang **sudah ada** diset ke start type `disabled`, sehingga konfigurasi yang gagal tidak jalan saat boot.
  Pemeriksaan: `sc.exe qc FerrumIndexer` → `START_TYPE: 4 DISABLED`.

Catatan: langkah service di installer MSI (`Return="ignore"`) dan NSIS (`nsExec`) tidak menggagalkan instalasi Ferrum
dan tidak memicu rollback MSI. Karena itu pembersihan dilakukan oleh `install` sendiri, seperti di atas. Setelah
penyebab diperbaiki (mis. ACL key atau token), jalankan `ferrum-indexer.exe install` sebagai administrator. Kalau
penghapusan atau penonaktifan juga gagal, pesan install memintanya menjalankan `ferrum-indexer uninstall`.

## 7. Uji otomatis (tanpa admin)

- `cargo test -p ferrum-indexer`: protokol (round-trip, strict decode, fuzz), config, policy (accept/setMode/rate
  limit), SDDL, ACE IU tanpa create-instance, verifikasi server (FER-480), probe pipe restricted, folder data (owner user tidak dipercaya untuk folder
  maupun file anak, junction tanam dipindah, folder yang muncul lagi tidak diadopsi, handle menahan rename, hardlink
  dan symlink pada conf/tmp/log dihapus tanpa menyentuh target; tes symlink dilewati tanpa Developer Mode), parser CLI, dan **IPC end-to-end** dengan server console di pipe dev
  (`tests/console_ipc.rs`: semua request, kebijakan mode + persistensi, frame rusak, rate limit, batas koneksi,
  FIRST_PIPE_INSTANCE, stop).
- `cargo test --lib indexer_client`: validasi `indexer_set_mode`, serialisasi status, status tanpa service.
- vitest: `src/lib/ipc/indexer.test.ts`, `src/lib/components/IndexerSettingsSection.test.ts`.
- Playwright (mock IPC): `e2e/indexer-settings.e2e.ts`.
- Manual cepat tanpa admin: `ferrum-indexer.exe --console --pipe dev1` lalu `ferrum-indexer.exe status --pipe dev1`.
- **S2 (FER-475)**, semuanya tanpa admin di atas volume sintetis (`ferrum_ntfs_scan::mft::fixture`, fitur
  `fixture`) dan journal sintetis byte-exact (`usn::fixture`):
  - `cargo test -p ferrum-ntfs-scan`: `scan_detailed` (mtime + atribut), `parse_live_record` (raw vs sudah
    fixed-up identik, gone/corrupt, extension record, batas 64 bacaan, fuzz).
  - `cargo test -p ferrum-indexer`: `index` (insert/hapus/replace, nama tidak valid, prune yatim/siklus/parent file,
    hardlink separuh di area dikecualikan, kompaksi), `usn` (V2/V3, skip V4, record rusak, fuzz), `persist`
    (round-trip, header/body rusak, budget sebelum inflate, body jahat ber-CRC benar, fuzz), `scope`, `engine`
    (build HP/shared, budget, tanpa journal, create/rename/modify/hardlink/delete/record dipakai ulang, replay
    idempoten, profil tidak pernah diindeks live, folder keluar profil → rebuild, "New folder"+rename tidak
    rebuild, folder profil dibuat ulang, wrap/ID berubah, buffer journal rusak, worker: simpan → muat → replay tanpa
    build ulang, rebuild karena wrap/ID/file rusak/cakupan, error buka + retry, budget, pause, ganti mode), `store`
    (save/load/remove/remove_all, hardlink tanam pada `.idx`/`.idx.tmp` tidak diikuti, folder `index` junction
    ditolak, purge, SID service), `volume` (deteksi drive fixed NTFS, budget, ProfileList, non-admin ditolak
    membuka volume), dan `tests/console_ipc.rs::index_status_over_the_pipe` (worker Windows asli: semua volume
    `access_denied` sebagai user biasa, pause tercermin).

## 8. Indeks MFT + USN (S2, FER-475)

### 8.1 Bentuk

| Bagian                                          | Lokasi (`src-tauri/crates/indexer/src/`)                                 |
| ----------------------------------------------- | ------------------------------------------------------------------------ |
| Indeks per volume di memori                     | `index.rs`                                                               |
| Parser output `FSCTL_READ_USN_JOURNAL`          | `usn.rs`                                                                 |
| Format file indeks (encode/decode murni)        | `persist.rs`                                                             |
| Cakupan per mode privasi + hook filter hasil S3 | `scope.rs`                                                               |
| Build, live update, pemulihan, worker           | `engine.rs` (+ `engine_tests.rs`), platform-netral                       |
| Sisi Windows (volume, FSCTL, profil, budget)    | `volume.rs`                                                              |
| File indeks di `ProgramData\Ferrum\index`       | `store.rs` (`IndexDir`)                                                  |
| Parser MFT (dipakai ulang, tidak diduplikasi)   | crate `ferrum-ntfs-scan` (FER-460): `scan_detailed`, `parse_live_record` |

Satu thread worker (`ferrum-indexer-index`) untuk semua volume, berjalan dengan `THREAD_MODE_BACKGROUND_BEGIN`
(prioritas CPU, I/O, dan memori terendah). Volume dibangun berurutan, bukan paralel, supaya beban disk dan memori
kecil.

### 8.2 Volume yang diindeks

Semua volume **lokal tetap (DRIVE_FIXED) ber-NTFS** yang punya huruf drive. Removable, jaringan, optik, RAM disk, dan
file system lain dilewati. Daftar volume dan profil dicek ulang tiap 5 menit. Identitas volume = huruf + serial
volume; file indeks dinamai `vol-<serial 16 hex>.idx`, jadi huruf drive yang berubah tidak memakai indeks yang salah.

### 8.3 Isi indeks dan struktur

Satu entri per pasangan (record MFT, nama), persis seperti scan MFT FER-460: hardlink = satu entri per nama, nama
DOS 8.3 dilewati, record metafile (0..15) dan isi `$Extend` tidak pernah masuk, nama tidak valid (`:`, kontrol,
`\`, `/`, `.`, `..`) ditolak. Per entri: file reference (record + sequence), parent, jenis (file/dir/link),
ukuran logis, alokasi, mtime (`$STANDARD_INFORMATION`, bukan salinan `$FILE_NAME`), atribut.

Struktur kompak: node 56 byte di satu `Vec` dengan free list, nama UTF-8 di satu arena (offset + panjang), tabel
`record → node` 4 byte per record MFT, nama-nama satu record dirantai. Arena dipadatkan kalau sampah > separuh arena
dan > 1 MiB. Insert dibatasi: nomor record ≤ 2^28, panjang nama, ukuran arena. Rantai parent ditelusuri dengan batas
kedalaman 4096 dan deteksi siklus; entri yang tidak tersambung ke root dipangkas, bukan disimpan sebagai yatim.

### 8.4 Build dan live update

1. **Build:** posisi journal (`FSCTL_QUERY_USN_JOURNAL`) diambil **sebelum** scan, lalu MFT dibaca lewat
   `ferrum_ntfs_scan::mft::scan_detailed` (parser yang sudah dikeraskan FER-460, tidak diduplikasi). Perubahan
   selama scan diputar ulang sesudahnya.
2. **Live:** tiap ~1 detik worker membaca journal (`FSCTL_READ_USN_JOURNAL`, `BytesToWaitFor = 0`, tidak memblok)
   dengan `ReasonMask` hanya untuk perubahan yang relevan (create, delete, rename, hardlink, data, basic info,
   kompresi/enkripsi, reparse, stream, close). Maksimal 256 bacaan per giliran.
3. **Aturan pembaruan:** record journal hanya berarti "record N berubah". Worker membaca ulang record N lewat
   `FSCTL_GET_NTFS_FILE_RECORD` dan mengganti semua yang diketahui tentang N dengan **keadaan record saat ini**
   (`ferrum_ntfs_scan::mft::parse_live_record`, terjemahan yang sama dengan scan). Satu aturan ini menangani create,
   delete, rename, hardlink, perubahan data dan atribut, record yang dipakai ulang dengan sequence baru, dan
   **idempoten**: memutar ulang record yang sudah tercermin tidak mengubah apa pun. Extension record dari attribute
   list residen ikut dibaca (maksimal 64). Attribute list non-residen (file yang sangat terfragmentasi) → nama dari
   base record dipakai dan ukuran lama dipertahankan (`incomplete`).
4. Record yang **gagal dibaca** (error OS) → posisi journal **tidak** dimajukan dan batch itu diulang pada poll
   berikutnya (idempoten, tidak ada update yang hilang). Gagal 3 poll berturut-turut → rebuild dari MFT. Kode OS
   pertama ditulis ke log. Record **rusak** (`Corrupt`, deterministik) mempertahankan entri lama dan dihitung.

Read-only total: volume dibuka persis seperti helper FER-460 (`GENERIC_READ`, share penuh) **per giliran** lalu
ditutup lagi, sehingga tidak ada handle volume jangka panjang yang menghalangi lock volume (chkdsk, dismount). Tidak
ada file di volume yang dibuka, jadi tidak ada hydration file cloud, oplock break, atau pembacaan isi file. Journal
**tidak pernah** dibuat, diubah ukurannya, atau dihapus; volume tanpa journal aktif menjadi snapshot (`live=false`)
dan dibangun ulang kalau journal kemudian aktif (dicek tiap poll).

### 8.5 Persistensi dan pemulihan

- File: `C:\ProgramData\Ferrum\index\vol-<serial>.idx`. Header 76 byte (magic `FRIXIDX\0`, versi, serial volume,
  journal ID, next USN, fingerprint cakupan, root FRN, jumlah entri, panjang body, CRC-32) + body deflate
  (`flate2`, backend Rust murni). Disimpan setelah build, lalu paling sering tiap 10 menit kalau ada perubahan, dan
  saat service stop.
- **I/O file sesuai aturan H1 S1** (`store::IndexDir`): folder `index` dibuat atomik dengan SD final dan ditahan tanpa
  `FILE_SHARE_DELETE`; file dibuka dengan `FILE_FLAG_OPEN_REPARSE_POINT` lalu dicek setelah dibuka (file biasa, link
  count 1, owner SYSTEM/Administrators); tulis = `.idx.tmp` baru (`CREATE_NEW` + SD eksplisit) → flush →
  rename-by-handle dengan replace; hapus lewat handle. File/link tanam atau yang lebih besar dari budget dihapus lewat
  handle-nya sendiri; nama asing tidak disentuh. `--purge-data` ikut menghapus indeks dan folder `index`.
- **Decode ketat:** panjang body dicek terhadap budget memori **sebelum** inflate, inflate dibatasi, CRC harus cocok,
  setiap entri divalidasi ulang oleh `VolumeIndex::add_name`, jumlah entri harus tepat, tanpa data sisa. Di-fuzz.
- **Saat start:** file dimuat hanya kalau serial volume, fingerprint cakupan (mode + profil yang dikecualikan) dan
  journal ID cocok, dan USN tersimpan masih di dalam journal (`FirstUsn ≤ USN ≤ NextUsn`). Lalu journal diputar
  ulang dari USN tersimpan.
- **Rebuild dari MFT** (file lama dihapus dulu) kalau: file rusak/tidak bisa di-decode, volume lain, cakupan berubah,
  journal wrap (`ERROR_JOURNAL_ENTRY_DELETED` atau USN di bawah `FirstUsn`), journal ID berubah atau journal sedang
  dihapus, buffer journal ditolak parser, atau folder yang keluar dari profil yang dikecualikan muncul dengan isi
  yang belum pernah diindeks (lihat 8.6), file record gagal dibaca 3 poll berturut-turut, atau journal muncul di
  volume snapshot.
- **Backoff:** rebuild pertama langsung; rebuild berikutnya dalam 30 menit ditunda 5 menit, 10, 20, … maksimal 1 jam,
  supaya journal yang wrap lebih cepat dari scan tidak membuat disk sibuk terus.

### 8.6 Mode `shared` dan privasi

- Isi setiap folder profil (`ProfileImagePath` semua entri `HKLM\...\ProfileList` di volume itu, termasuk profil
  service) **tidak diindeks**. Folder profilnya sendiri tetap ada (namanya memang terlihat semua user di
  `C:\Users`). `C:\Users\Public` bukan entri ProfileList, jadi tetap diindeks.
- **`$Recycle.Bin` di semua volume** juga tidak diindeks isinya (review M1, keputusan desain: seluruh folder,
  bukan per SID). Folder yang dihapus dari profil berpindah ke `$Recycle.Bin\<SID>\$R…` dengan nama isinya utuh;
  pemindahan itu diabaikan (tidak memicu rebuild), dan rebuild pun tidak mengindeksnya.
- **Daftar profil fail closed** (review M2): `ProfileList` yang tidak bisa dibuka, entri yang tidak bisa dibaca,
  path kosong/terlalu panjang (> 32 767 unit), atau > 4096 entri → di mode shared **tidak ada volume yang dibangun**:
  indeks di memori dibuang, status `error / profiles_unavailable`, log `profile list unavailable (<alasan>)` tanpa
  path, dicoba lagi tiap 5 menit. File indeks yang sudah ada (dibuat dengan daftar lengkap sebelumnya) tidak dimuat
  selama daftar tidak terbaca. Mode `high_performance` tidak membutuhkan daftar ini.
- **Path final** (review M3): tiap profil di-resolve dengan `GetFinalPathNameByHandleW` (handle
  `FILE_READ_ATTRIBUTES` + `BACKUP_SEMANTICS`, tidak membaca isi) sehingga junction/symlink, termasuk `C:\Users` yang
  di-junction ke `D:\Users`, dikecualikan di volume tujuan yang benar; path literal juga tetap dikecualikan. Profil
  yang tidak ada di disk dilewati (path literalnya tetap dikecualikan bila dibuat kemudian). Profil yang ada tetapi
  tidak bisa di-resolve → seluruh daftar gagal → fail closed seperti di atas. Volume ber-huruf yang **juga di-mount di
  dalam profil** (`GetVolumePathNamesForVolumeNameW`) tidak diindeks sama sekali di mode shared (`error /
private_volume`, file indeksnya dihapus).
- Selama build, MFT tidak berurutan parent-dulu, jadi nama di bawah profil sempat ada di memori proses SYSTEM sampai
  scan selesai (atau sampai budget memori tersentuh: isi profil yang sudah bisa dipastikan lalu dipangkas lebih awal),
  lalu dipangkas sebelum indeks dipakai atau disimpan. Tidak pernah disimpan ke disk atau dikirim.
- Live: entri baru di bawah profil diabaikan; folder publik yang dipindah **ke dalam** profil dipangkas beserta
  isinya; folder yang dibuat, di-rename, atau dipindah **ke** path profil langsung jadi pengecualian dan isinya yang sudah terindeks langsung dipangkas; record folder profil yang dihapus lalu dipakai ulang tidak lagi dikecualikan; folder
  yang dipindah **keluar** dari profil (isinya belum pernah diindeks) memicu rebuild. Folder baru ("New folder" +
  rename) tidak memicu rebuild.
- **Ganti mode** (`setMode`): semua indeks di memori dan **semua file indeks dihapus** (juga kalau service di-stop
  tepat setelahnya; indeks cakupan lama tidak pernah disimpan setelah mode berubah), lalu dibangun ulang dalam
  cakupan baru. Dari `high_performance` ke `shared`, file yang memuat nama profil user lain dihapus (penghapusan file
  biasa; datanya tidak di-wipe, folder hanya bisa dibaca SYSTEM/Administrators).
- **Hook S3:** diganti di FER-476 oleh `query::DirAccess` (AccessCheck klien, §9.3).

### 8.7 Batas sumber daya

- **Budget memori** semua indeks: 1/8 RAM fisik, minimal 256 MiB, maksimal 2 GiB (`volume::memory_budget`). Dicek
  selama build (tiap 4096 record) dan setelah tiap batch journal. Melewati budget → volume `error / memory_limit`,
  indeksnya dibuang, dicoba lagi 5 menit kemudian. Pemakaian per volume dan total dilaporkan di `indexStatus`.
- CPU/I/O: thread background mode, polling 1 detik, build berurutan.
- **Pause** (`sc pause`): worker berhenti total (tidak membaca journal, tidak membangun). Build yang sedang jalan
  **dihentikan** di checkpoint berikutnya (maks 4096 record) dan handle volume ditutup, supaya lock volume/dismount
  (`chkdsk /f`) tidak terhalang selama pause (review L7); build diulang dari awal setelah continue. Continue
  melanjutkan live update dari USN terakhir. Ganti mode tetap diproses saat pause: indeks di memori dan file indeks
  cakupan lama langsung dihapus, build ulang menunggu continue (review L2). Stop: worker berhenti, menyimpan indeks
  yang berubah, lalu keluar.
- Volume gagal (akses ditolak, bukan NTFS, layout MFT aneh, baca gagal) → `error` dengan kode, dicoba lagi tiap 5
  menit.

### 8.8 IPC v2

`PROTO_VERSION = 2` (Ferrum dan service selalu dikirim bersama di satu installer; versi lain ditolak saat hello).

- `status.index` sekarang agregat: `none` (belum ada volume), `building`, `ready`, `error` (semua volume gagal).
- Request baru `indexStatus` (tanpa payload) → `paused`, `memory_bytes`, `memory_limit`, dan per volume (maks 26):
  huruf, state `building|ready|error`, kode error, jumlah entri, memori, journal ID, next USN, `live`, progres build
  0..1000. Hanya kode, tanpa nama file atau path. Frame terbesar (26 volume) ±1 KiB, jauh di bawah batas 4 KiB.
  Decoder ketat (huruf A–Z, enum, progres ≤ 1000) dan ikut di-fuzz.
- CLI: `ferrum-indexer.exe index-status [--pipe S]`; console: `--console --pipe S --index`.
- Query search/subtree: IPC v3 (S3, §9).

### 8.9 Privilege dan service SID

- **RequiredPrivileges tetap `SeChangeNotifyPrivilege` saja.** Membuka `\\.\C:` butuh keanggotaan Administrators di
  token (SYSTEM punya; RequiredPrivileges tidak membuang grup). `FSCTL_QUERY_USN_JOURNAL`, `FSCTL_READ_USN_JOURNAL`,
  `FSCTL_GET_NTFS_FILE_RECORD`, dan `FSCTL_GET_NTFS_VOLUME_DATA` dipakai FER-460 dari token admin elevated yang semua
  privilege-nya disabled, jadi tidak membutuhkan privilege. Kalau verifikasi manual menunjukkan `access_denied` atau
  log `file record reads failed (first OS error 5)`, privilege yang disebut di log ditambahkan ke
  `REQUIRED_PRIVILEGES` dan diaktifkan secara eksplisit, lalu didokumentasikan.
- **Service SID:** SD folder data, folder `index`, file yang dibuat service, dan DACL pipe memberi akses ke
  `NT SERVICE\FerrumIndexer` (`store::SERVICE_SID`, diturunkan dari SHA-1 nama service; diuji).
- **FER-476: tipe SID `Restricted` jadi default** (`service::SERVICE_SID_TYPE`), sesuai keputusan desain
  sebelum S3 menambah query IPC. Rinciannya:
  - **Folder data dibuat installer.** `ProgramData` tidak memberi hak create ke SID pembatas. Jadi folder
    `C:\ProgramData\Ferrum` dibuat oleh `install` (elevated, token tidak dibatasi), bukan oleh service. Kalau folder
    itu dihapus saat service terpasang, service berjalan tanpa persistensi sampai `install`/repair berikutnya.
  - **`install` menyegel ulang file lama** (`DataDir::reseal`). SD baru diterapkan ulang ke `indexer.conf`, log, dan
    `vol-*.idx`. File dari S1 belum punya ACE service SID, sehingga token write-restricted tidak bisa append/rotate
    log (tes `reseal_updates_old_file_sds`).
  - **Service tidak lagi memaksa `WRITE_DAC`.** Kalau `WRITE_DAC`/`WRITE_OWNER` pada folder ditolak (SD lama), folder
    tetap dipegang tanpa menerapkan ulang SD (`open_dir_maybe_fix`).
  - **Probe saat install** (`service::probe_restricted`). Dengan salinan token installer yang dibuat
    write-restricted (SID pembatas = service SID, Everyone, WRITE RESTRICTED), `install` memegang folder data,
    append log, membuka folder `index`, lalu menjalankan `pipe::probe_instances`: dua instance pipe buangan
    `\\.\pipe\FerrumIndexer-probe-<pid>-<n>` di namespace yang **sama** dengan pipe produksi (FER-480; sebelumnya
    probe memakai `ProtectedPrefix` dan gagal di PC nyata). DACL-nya DACL produksi ditambah satu ACE GA untuk admin
    pembuat, yang menggantikan SYSTEM. Probe juga memeriksa owner pipe dengan aturan klien. Fungsi yang sama diuji
    tanpa admin (`pipe::tests::restricted_probe_succeeds_in_the_service_namespace`). Kalau semuanya berhasil, SID
    didaftarkan `restricted`
    (output: `service SID type: restricted (probe ok)`). Kalau ada yang gagal, SID didaftarkan `unrestricted` dan
    pesannya (`WARNING: restricted service SID probe failed (...)`) ditulis ke output dan `indexer.log`.
    `indexer-verify.ps1` lalu FAIL di baris SID.
  - **Tes tanpa admin** di emulasi yang sama: `store::tests::restricted_service_token_can_use_the_data_directory`
    (hold, config, log + rotasi, save/load indeks) dan `restricted_token_needs_the_service_sid_ace` (kontrol
    negatif).
  - **Yang hanya bisa diuji di instalasi nyata:** token SYSTEM sungguhan, pembuatan pipe `\\.\pipe\FerrumIndexer`
    oleh service, dan verifikasi klien terhadap PID/image service sungguhan. Lihat §8.10 langkah 9–11 dan §8.11.
  - **Temuan FER-480 (instalasi nyata build 3678969):** probe gagal dengan `pipe: Access is denied (os error 5)`,
    SID didaftarkan `unrestricted`, dan service `degraded`. Penyebabnya, NPFS menolak pembuatan pipe di
    `ProtectedPrefix\Administrators\` oleh token write-restricted. Bukti dan perbaikannya ada di §8.11.
  - Opsi lain yang tidak dipilih: tetap `Restricted` walaupun probe gagal (lebih ketat, tetapi service bisa tidak
    jalan). Ganti ke opsi itu cukup dengan menghapus fallback di `install`.

### 8.10 Verifikasi manual S2 (owner, butuh admin)

Sesi agent tidak memasang service dan tidak menjalankan apa pun sebagai admin. Langkah untuk owner (setelah langkah
§6 S1 lolos):

1. Pasang build ini (MSI atau NSIS), pilih mode **High performance**.
2. Tunggu 1–2 menit. PowerShell biasa: `& "C:\Program Files\Ferrum\ferrum-indexer.exe" index-status`. Harus muncul
   semua drive fixed NTFS; tiap drive akhirnya `state=ready ... live=true`, `entries` dalam orde jutaan untuk C:,
   `memory` wajar (±100 byte per entri).
3. PowerShell **elevated**: `powershell -ExecutionPolicy Bypass -File scripts\indexer-verify.ps1`. Semua PASS,
   termasuk ACL `ProgramData\Ferrum\index` dan isinya hanya `vol-<serial>.idx`.
4. **Live:** catat `entries` dan `next_usn` C:, buat file baru di Desktop, tunggu 2–3 detik, `index-status` lagi →
   `entries` +1 dan `next_usn` naik. Hapus file → `entries` kembali.
5. **Task Manager → Details → ferrum-indexer.exe:** CPU ±0 saat idle; memori kira-kira sama dengan total `memory`.
   Kolom "Priority" thread tidak terlihat di Task Manager; opsional dengan Process Explorer → Threads: thread
   `ferrum-indexer-index` berprioritas Background/Idle.
6. **Persistensi:** `sc.exe stop FerrumIndexer`, buat satu file, `sc.exe start FerrumIndexer`. `index-status`
   segera `ready` (tanpa build ulang lama) dan file baru terhitung. `C:\ProgramData\Ferrum\indexer.log` berisi
   `saved index loaded ... replaying journal`.
7. **Pause:** `sc.exe pause FerrumIndexer` → `index-status` `paused=true`; buat file → `next_usn` tidak berubah.
   `sc.exe continue FerrumIndexer` → beberapa detik kemudian file terhitung.
8. **Mode shared:** Settings → Indexer → Ganti ke Bersama. `index-status` sebentar `building`, lalu `ready` dengan
   `entries` lebih kecil. Log: `mode changed: indexes dropped`. Opsional (elevated): file indeks lama terhapus lalu
   ditulis ulang.
9. **Service SID restricted (FER-476, default; probe diperbaiki FER-480).** Jalankan installer dari PowerShell
   **elevated** supaya outputnya terlihat, atau baca `C:\ProgramData\Ferrum\indexer.log` setelahnya. Di log harus ada
   baris `install: restricted service SID probe` **tanpa** baris `probe failed` sesudahnya, lalu
   `listening on \\.\pipe\FerrumIndexer (owner S-1-5-18)` (review F2: owner SYSTEM eksplisit; owner lain berarti fallback `pipe owner O:SY refused` di log, laporkan), dan **tidak ada**
   `search/subtree disabled (degraded)`. `sc.exe qsidtype FerrumIndexer` harus menampilkan
   `SERVICE_SID_TYPE: RESTRICTED`. `indexer-verify.ps1` memeriksa semuanya.
   - Kalau muncul `WARNING: restricted service SID probe failed (...)`, catat teks di dalam kurung dan laporkan di
     FER-480. Service tetap berjalan `unrestricted`, dan `indexer-verify.ps1` FAIL di baris SID.
10. Dengan `restricted`, ulangi langkah 2, 4, 6 dan `ferrum-indexer.exe status`. Semua harus normal, dan
    `indexer.log` harus terus bertambah. `status` **tidak** boleh menyebut `degraded`, dan Settings → Indexer tidak
    boleh menampilkan "Mode terbatas".
    - Kalau service gagal start, pipe tidak ada, atau log/indeks tidak tertulis, kembalikan dulu dengan
      `sc.exe sidtype FerrumIndexer unrestricted` lalu `sc.exe stop`/`start FerrumIndexer`. Setelah itu laporkan error
      dari Event Viewer dan `indexer.log`.
    - **Rollback ke unrestricted berarti query mati.** Service mendeteksi token yang tidak dibatasi, lalu menjawab
      search/subtree dengan `degraded`. Settings → Indexer menampilkan "Mode terbatas (degraded)", dan search serta
      Disk Usage memakai scan biasa. Ini disengaja: query dari user lain tidak boleh dilayani oleh token SYSTEM yang
      tidak dibatasi. Status dan mode tetap bisa dipakai.
    - Installer berikutnya akan memprobe dan mendaftarkan ulang tipe SID. Restart service setelah
      `sc.exe sidtype FerrumIndexer restricted` menyalakan query lagi.
11. Tiga uji tambahan dari review, semuanya dengan `restricted` (default):
    - **11a. Instalasi baru atau setelah purge** (`ProgramData\Ferrum` belum ada):
      1. Uninstall dengan purge, lalu pastikan `C:\ProgramData\Ferrum` sudah hilang.
      2. Pasang lagi.
      3. Cek hasilnya. Service harus RUNNING dengan SID RESTRICTED, `indexer.conf` dan `indexer.log` ada, dan
         `index-status` ready. Folder itu dibuat oleh `install`, bukan oleh service.
    - **11b. Upgrade dari instalasi S1** (SD folder data dan log belum memuat ACE service SID):
      1. Pasang build S1, lalu pasang build ini di atasnya.
      2. Cek `icacls C:\ProgramData\Ferrum` dan `icacls C:\ProgramData\Ferrum\indexer.log`. Keduanya harus memuat
         `NT SERVICE\FerrumIndexer` (hasil `reseal`).
      3. Pastikan log tetap bertambah dan indeks tersimpan.
    - **11c. Pipe dan rotasi log:**
      1. Pastikan `ferrum-indexer.exe status` dan Settings → Indexer berfungsi (pipe `\\.\pipe\FerrumIndexer`
         dibuat oleh token restricted, dan klien memverifikasinya).
      2. Buka dua jendela Ferrum sekaligus, atau jalankan `status` berulang saat Ferrum terbuka. Koneksi kedua dan
         seterusnya juga harus dilayani. Ini menguji penambahan instance pipe oleh token restricted.
      3. Buat `indexer.log` lebih dari 1 MiB, misalnya dengan mengulang `sc pause`/`continue`, atau menyalin file besar
         ke `indexer.log` saat service berhenti (elevated). Lalu start service. Harus muncul `indexer.log.1`, dan log
         baru tetap tertulis.
12. **Pemulihan wrap/ID (opsional, mengubah journal volume!):** hanya di volume uji, bukan C:. `fsutil usn
deletejournal /d X:` lalu `fsutil usn createjournal m=33554432 a=4194304 X:` saat service berjalan → log
    `rebuild from MFT (JournalChanged)` dan `index-status X` kembali `ready`.
13. **Purge:** uninstall dengan purge (§6) → folder `C:\ProgramData\Ferrum\index` ikut hilang.

Kalau ada volume `state=error`: kode `access_denied` = token service tidak bisa membuka volume (lihat 8.9);
`memory_limit` = budget terlampaui (catat `entries`/`memory` dan RAM PC); `profiles_unavailable` = mode shared tanpa daftar profil lengkap (fail closed, lihat log); `private_volume` = volume di-mount di dalam profil (disengaja, mode shared); `mft_layout`/`read_failed` = laporkan
kode dan drive di FER-475.

### 8.11 FER-480: pipe untuk service SID Restricted

**Temuan (instalasi nyata, build 3678969).** `indexer.log`:
`install: restricted service SID probe failed (pipe: Access is denied. (os error 5)); registering unrestricted`, lalu
`service token is not restricted by the service SID: search/subtree disabled (degraded)`, lalu
`listening on \\.\pipe\ProtectedPrefix\Administrators\FerrumIndexer`. `sc qsidtype` = UNRESTRICTED, dan `status`
menunjukkan `degraded=true`. Pengaman FER-476 bekerja sesuai desain, tetapi query mati.

**Akar masalah.** Membuat pipe di bawah `ProtectedPrefix\Administrators\` gagal untuk token write-restricted, walaupun
DACL pipe memberi akses ke service SID. Buktinya:

- Probe gagal di langkah **pertama**: `pipe:` adalah instance pertama, sedangkan instance kedua akan berbunyi
  `pipe instance:`. Sebelum itu, langkah folder data, log, dan `index` di bawah token yang sama berhasil (baris
  `install: restricted service SID probe` tertulis).
- Token yang sama tanpa restriksi bisa membuat pipe itu. Service `unrestricted` mendengarkan di
  `ProtectedPrefix\Administrators\FerrumIndexer`, dan admin elevated juga bisa.
- Token write-restricted yang sama di **akar NPFS** bisa membuat instance pertama dan kedua.
  `scripts\indexer-pipe-probe.ps1` (user standar):
  `write-restricted \\.\pipe\FerrumProbe-<pid> first=OK, second=OK`. Tes
  `restricted_probe_succeeds_in_the_service_namespace` dan `restricted_token_can_add_instances_only_with_service_ace`.
- Jadi NPFS memeriksa keanggotaan Administrators untuk prefix itu sebagai pemeriksaan akses tulis. Token
  write-restricted gagal di pemeriksaan kedua (daftar SID pembatas tidak memuat Administrators), dan DACL pipe tidak
  bisa memperbaikinya. Konfirmasi elevated (tidak bisa dijalankan sesi agent):
  `scripts\indexer-pipe-probe.ps1` dari PowerShell **elevated** harus menunjukkan
  `own token ProtectedPrefix… first=OK` dan `write-restricted ProtectedPrefix… first=ERR 5`.
- Unit test lama tidak menangkapnya karena memakai pipe dev di akar NPFS, bukan `ProtectedPrefix`.

**Keputusan desain.** Pipe `\\.\pipe\FerrumIndexer` di akar NPFS, satu nama untuk semua tipe SID, dengan
`FILE_FLAG_FIRST_PIPE_INSTANCE`, `PIPE_REJECT_REMOTE_CLIENTS`, dan DACL yang sama. Anti-squatting dilakukan lewat
verifikasi server di klien (§3 butir 1). Probe install menguji namespace dan fungsi pembuatan pipe yang benar-benar
dipakai. Alternatif "tetap di `ProtectedPrefix` tetapi dibuat dengan cara lain" ditolak:

- token service Restricted tidak pernah tanpa restriksi;
- prefix `LocalService`/`NetworkService` mensyaratkan akun lain;
- membuat pipe dari proses lain lalu mewariskan handle menambah proses SYSTEM tak terbatas, padahal itulah yang mau
  dihindari.

**Kompatibilitas.** Klien baru tidak lagi membuka nama `ProtectedPrefix`. Service lama (3678969) tidak terlihat
oleh Ferrum baru, sehingga Ferrum memakai scan biasa sampai installer baru memasang service baru. Installer selalu
memasang keduanya bersamaan.

**Langkah uji user (elevated hanya di langkah yang disebut):**

1. (Opsional, bukti akar masalah, **sebelum** upgrade) PowerShell elevated:
   `powershell -ExecutionPolicy Bypass -File scripts\indexer-pipe-probe.ps1`. Kirim keempat baris hasilnya.
2. Pasang ulang dengan installer build FER-480 (MSI atau NSIS). Mode tetap.
3. PowerShell biasa: `Get-Content C:\ProgramData\Ferrum\indexer.log -Tail 30` (kalau ditolak, pakai elevated).
   Harus ada `install: restricted service SID probe` **tanpa** `probe failed`, lalu
   `listening on \\.\pipe\FerrumIndexer (owner …)`, dan **tidak ada** `search/subtree disabled (degraded)`.
4. `sc.exe qsidtype FerrumIndexer` → `RESTRICTED`.
5. PowerShell biasa: `& "C:\Program Files\Ferrum\ferrum-indexer.exe" status` → `degraded=false`. `index-status`
   → C: `ready`.
6. PowerShell biasa: `powershell -ExecutionPolicy Bypass -File scripts\indexer-pipe-probe.ps1 -Client`. Pada baris
   `client \\.\pipe\FerrumIndexer`, `serverPid` harus sama dengan `SCM FerrumIndexer PID`.
7. Ferrum → Settings → Indexer: Berjalan, tanpa "Mode terbatas". Search seluruh drive C: dan Disk Usage C: harus
   instan (dari indeks).
8. PowerShell **elevated**: `scripts\indexer-verify.ps1` → semua PASS (termasuk SID RESTRICTED, pipe
   `\\.\pipe\FerrumIndexer`, owner SYSTEM/Administrators, server PID = PID SCM, status tidak degraded).
9. Squatting (opsional): §6 langkah 11.

## 9. Query, Ferrum, dan AI (S3, FER-476)

### 9.1 IPC v3

`PROTO_VERSION = 3`. Request maks 2 KiB, response maks 1 MiB (dicek sebelum alokasi), decoder ketat dan ikut di-fuzz.

- **`search`**: pola (≤ 256 byte, tanpa `\ / :` dan karakter kontrol), cara cocok `substring | prefix | glob`
  (`*`, `?`, seluruh nama), scope (kosong = semua volume, atau path absolut `X:\...` dengan nama valid, ≤ 1024 byte),
  ekstensi (≤ 16, huruf kecil/angka), jenis (file/folder/keduanya), `limit` 1..1000, dan cursor (huruf + slot).
  Semuanya case-insensitive.
  - Jawaban berupa satu halaman hit: jenis, ukuran, alokasi, mtime, atribut, dan path lengkap (≤ 4096 byte).
  - Jawaban juga membawa cursor berikutnya, flag `stale`, mask volume tanpa indeks, dan flag `excluded_inside`
    (scope memuat area yang dikecualikan).
  - Satu halaman ≈ 256 KiB.
- **`subtree`**: path absolut sebuah folder. Jawabannya dikirim bertahap (_stream_) sebagai frame `SubtreeChunk`
  (≈ 256 KiB, ≤ 16 384 entri), dengan `done` di frame terakhir.
  - Isinya setiap entri yang terlihat di bawah folder itu: frn, parent, jenis, ukuran, alokasi, nama.
  - Ferrum membangunnya menjadi snapshot Disk Usage yang sama dengan jalur MFT FER-460.
- **Kode error baru:** `not_indexed`, `timeout`, `too_large`.
  - `not_indexed` berarti salah satu dari: tidak ada, tidak bisa di-list oleh klien, di area yang dikecualikan, atau
    volumenya belum punya indeks.
  - Semua kasus itu dijawab dengan kode yang sama.
  - **Batasan (review L2):** waktu jawabnya masih bisa membedakan. `find_dir_path` berhenti di kecocokan pertama,
    sehingga scope tersembunyi yang ada dijawab dalam hitungan µs, sedangkan scope yang tidak ada memindai seluruh
    indeks (ms). Ini setara dengan perilaku Windows sendiri untuk path tebakan (dengan bypass traverse, `CreateFile`
    menjawab ACCESS_DENIED vs NOT_FOUND), jadi diterima. Kode jawabannya tetap tidak membocorkan.
- **`degraded`** (review H1): query dimatikan karena token service tidak restricted (§2). Ferrum memakai scan
  biasa.
  - Ferrum lalu memakai scan biasa.

### 9.2 Batas kerja

| Batas                          | Nilai                                                                                                                                                                                                                                             |
| ------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Lock baca indeks per pegangan  | ≤ 32 768 slot per pegangan saat memindai, **tidak pernah** dipegang saat menulis ke klien. Pengecualian: resolusi scope (`find_dir_path`) dan `excluded_inside` memegang lock baca sekali selama O(N) di awal query                               |
| Lock tulis worker per pegangan | ≤ 16 bacaan journal (query menyela backlog panjang)                                                                                                                                                                                               |
| Waktu satu `search`            | 1,5 detik, lalu halaman parsial + cursor                                                                                                                                                                                                          |
| Waktu satu `subtree`           | 120 detik, lalu `timeout`; maks 30 juta entri, lalu `too_large`                                                                                                                                                                                   |
| Query berat bersamaan          | 2 untuk seluruh service, 1 per SID user (lebih → `busy`); tetap di bawah batas laju per koneksi (§3)                                                                                                                                              |
| Klien lambat                   | write timeout 5 detik memutus koneksinya sendiri; worker dan klien lain tidak tertahan                                                                                                                                                            |
| Ganti mode                     | daftar indeks yang dilayani dikosongkan, dan query yang sedang berjalan dibatalkan (`not_indexed`). Daftar, epoch, dan mode diambil sebagai satu snapshot di bawah lock `mode` (review M1). Abort juga dicek sebelum setiap halaman dikembalikan. |

### 9.3 Mode Bersama: AccessCheck

- **Aturan visibilitas.** Sebuah nama hanya terlihat kalau **setiap** folder dari root volume sampai folder
  induknya bisa di-list oleh klien (`FILE_LIST_DIRECTORY`).
  - Nama folder yang tidak bisa di-list tetap terlihat (seperti di Explorer), tetapi isinya tidak.
  - Isi yang tidak terlihat juga tidak ikut dihitung di `subtree`/Disk Usage.
- **Pemeriksaan per folder** (`access.rs`):
  - Folder dibuka **lewat file ID** (`OpenFileById` dengan frn termasuk sequence number). Tidak pernah lewat path,
    jadi junction atau rename tidak bisa membelokkan pemeriksaan, dan record yang dipakai ulang gagal dibuka.
  - Hak yang diminta hanya `READ_CONTROL`.
  - SD folder dievaluasi dengan `AccessCheck` memakai token klien. Token itu token impersonation level
    _identification_ yang diambil saat hello, jadi service tidak pernah bertindak sebagai user.
  - Setiap kegagalan berarti "tidak boleh" (fail closed).
- **Cache dan hasilnya.**
  - **Kunci cache = identitas token untuk AccessCheck** (review N1/N1b). Isinya: logon session, user SID **beserta
    atributnya** (user SID deny-only adalah identitas lain), integrity level, setiap SID grup beserta atributnya
    (enabled, deny-only, mandatory, integrity, …, diurutkan), SID pembatas, flag AppContainer dan capability, serta
    device groups. Kunci disimpan sebagai byte kanonik utuh, bukan hash, jadi tidak ada tabrakan.
    - **Klaim** (user/device claim attributes, untuk conditional ACE) tidak diserialisasi. Token yang punya klaim,
      atau yang device groups/klaimnya tidak bisa dibaca, mendapat kunci unik per koneksi, sehingga keputusannya
      tidak pernah dipakai bersama.
    - Tes regresi:
      - folder yang hanya bisa di-list lewat Authenticated Users tetap ditolak untuk token yang AU-nya deny-only;
      - folder yang hanya bisa di-list oleh SID user tetap ditolak untuk token yang user SID-nya deny-only
        (`CreateRestrictedToken` SidsToDisable).

      Keduanya diuji setelah cache dihangatkan oleh token penuh.

    - Kalau identitas token tidak bisa dibaca, klien ditolak (`unauthenticated`) sebelum hello selesai. Tidak ada kunci
      cadangan bersama (review N2).
    - **Batas memori** (review N3/N4):
      - per identitas maksimal 200 000 keputusan (yang kedaluwarsa dibuang dulu, lalu hanya map identitas itu);
      - maksimal 4 identitas per SID user (identitas kelima menggusur yang paling lama dari user itu) dan 64
        identitas total;
      - maksimal 400 000 keputusan total, dengan identitas yang paling lama tidak dipakai digusur lebih dulu (tidak
        pernah identitas yang sedang ditulis);
      - keputusan kedaluwarsa disapu setiap 60 detik.
    - TTL 60 detik per keputusan (review L5).
  - **AccessCheck tidak pernah dijalankan sambil memegang lock baca indeks** (review N3). Di bawah lock hanya dipakai
    keputusan yang sudah diketahui. Kalau ada folder yang belum punya keputusan, scan berhenti di slot itu, lock
    dilepas, AccessCheck dijalankan, lalu scan dilanjutkan dari slot yang sama. Ini berlaku untuk search, resolusi
    scope, dan subtree, dan diuji dengan fake yang mengambil lock tulis di dalam setiap AccessCheck.
  - **Visibilitas folder induk dievaluasi sebelum nama dicocokkan** (review L3). Pekerjaan AccessCheck sebuah query
    hanya bergantung pada pohon folder, tidak pada apakah nama tersembunyi cocok dengan pola. Akibatnya search
    pertama sebuah sesi logon memeriksa setiap folder yang berisi entri; berkat cache bersama, search berikutnya
    murah.
  - Cache dibuang kalau journal membawa `SECURITY_CHANGE` (review I5: `ReasonMask` = relevan + `SECURITY_CHANGE`,
    epoch keamanan).
  - Diuji dengan AccessCheck sungguhan tanpa admin. Di `console_ipc.rs` ada folder yang bisa di-traverse tetapi
    tidak bisa di-list: di mode shared isinya hilang dari hasil search dan subtree, scope-nya menjawab
    `not_indexed`, dan di mode high_performance isinya terlihat.
- **Mode `high_performance`** tidak memakai filter.
- **Epoch keamanan** (cache AccessCheck dibuang) naik saat journal membawa `SECURITY_CHANGE`, saat indeks baru
  selesai dibangun atau dimuat (journal tidak diikuti selama rebuild atau stale), dan saat service di-continue
  setelah pause (review L5).
- **Query berat:** maksimal 2 sekaligus untuk seluruh service, dan maksimal 1 per SID user (review L4). Satu user
  tidak bisa mengambil semua slot.
- **`indexStatus` untuk non-admin di mode Bersama dibulatkan** (review I3): entri per 1000, `next_usn` per 16 MiB,
  memori per MiB.

- **Risiko diterima (FER-480 review F1): squatting nama pipe = DoS indeks.** Sejak pipe dipindah ke akar NPFS
  (§3, §8.11), proses lokal mana pun, termasuk user non-admin, bisa mengambil nama `\\.\pipe\FerrumIndexer`. Celahnya
  terbuka sebelum service delayed-start, saat restart atau recovery, dan saat upgrade. Akibatnya:
  - Di mode Bersama, **satu user bisa mematikan indeks untuk semua user** di PC itu.
  - **Tidak ada kebocoran data dan tidak ada eskalasi.** Semua klien memverifikasi server sebelum mengirim byte
    pertama (owner, PID SCM, image, token), sehingga squatter tidak menerima SID, query, atau hasil.
  - Semua klien memakai scan biasa. Settings dan AI menyebut "Nama pipe FerrumIndexer dipakai proses lain (PID n)".
  - Service mencatat PID, owner, dan image pemegang nama di `indexer.log` (±1 baris per 5 menit), sehingga admin
    bisa menindak.
  - Service pulih sendiri ≤2 detik setelah squatter pergi.

  Keputusan desain: risiko ini diterima. **Follow-up (belum dikerjakan):** fallback nama pipe acak per boot, yang
  diumumkan lewat lokasi yang hanya bisa ditulis SYSTEM/installer tetapi bisa dibaca user.

### 9.4 Follow-up review FER-475

- **L4.** Indeks tersimpan di-decode secara streaming: inflate per entri dan CRC inkremental. Body tidak lagi
  ditampung utuh. Puncak memori = file terkompresi + indeks.
- **L5.** Rebuild yang dipicu journal (wrap, ID baru, subtree belum terindeks, data rusak, record gagal dibaca):
  - Indeks lama tetap dilayani (`stale`) sampai indeks baru selesai, termasuk selama backoff.
  - Indeks lama dibuang kalau scope atau mode berubah, atau kalau terjadi error.
  - Batasan: selama rebuild satu volume, memori bisa sementara melewati budget sebesar satu indeks volume itu (lama
    - baru).
- **L1.** Profil baru dikecualikan lebih cepat:
  - `ProfileList` dipantau dengan `RegNotifyChangeKeyValue` (thread-agnostic, di-arm ulang). Profil baru memicu
    discovery pada poll berikutnya (±1 detik), bukan 5 menit.
  - Selain itu, daftar profil **dibaca ulang tepat sebelum setiap save** di mode shared. Kalau daftarnya berubah,
    save dibatalkan dan discovery diulang.
  - Sisa jendela yang diterima: ±1 detik di memori, tidak pernah ke disk atau ke klien sebelum prune.
- **L6.**
  - Prune live dijalankan sekali per pegangan lock catch-up, bukan per batch, dan selalu sebelum lock dilepas.
  - Prune saat build dijalankan hanya setelah memori bertambah ⅛ budget sejak prune sebelumnya.

### 9.5 Ferrum

- **Search** (`search::run_search_auto`):
  - Indeks dipakai untuk pencarian glob rekursif tanpa filter isi, di root drive lokal yang ada di indeks.
  - Aturan walker diterapkan ulang pada hit: nama (termasuk case-sensitive), ukuran, waktu, dan kedalaman.
  - Pencarian lain, service tidak ada/mati, atau error sebelum halaman pertama → walker lama, persis seperti
    sebelumnya.
  - Di mode Bersama, profil milik user sendiri (kalau ada di dalam root) di-walk lalu digabung.
  - Status bar search menampilkan "via indeks".
- **Disk Usage:** tawaran scan menambah tombol **Instan (indeks)** kalau indeks drive itu siap (cek murah, tanpa
  UAC).
  - Hasilnya memakai snapshot, tree, treemap, dan aksi yang sama dengan jalur lain. Badge engine bertuliskan
    "Indeks".
  - Notice muncul kalau indeks `stale`, atau kalau isi profil tidak dihitung (mode Bersama).
  - Kalau indeks gagal, scan biasa dijalankan dengan notice alasannya.
- **Settings → Indexer** menampilkan status indeks apa adanya:
  - tidak terpasang,
  - service mati,
  - dijeda,
  - sedang membangun (persen per drive),
  - siap,
  - gagal (kode per drive).

### 9.6 AI lewat chat

- **`indexer.status`** (read-only) menjawab kesiapan indeks: tidak terpasang, mati, membangun (persen per drive),
  siap, atau error. Jawabannya hanya berisi kode, tanpa path.
- **`indexer.setMode`** selalu lewat kartu konfirmasi di chat yang menjelaskan konsekuensi privasi.
  - Menaikkan ke High performance butuh centang "Saya mengerti, PC ini hanya dipakai oleh saya." (ditegakkan di
    kartu, store, dan executor), lalu UAC sesuai FER-474.
  - Menurunkan ke Bersama tidak butuh UAC.
  - Model hanya memilih mode tujuan.
- **`copy.adjust`** (FER-471): menaikkan/menurunkan thread copy dari chat diverifikasi end-to-end
  (`chatControlE2E.test.ts`).

### 9.7 Verifikasi manual S3 (owner)

Lakukan setelah §6 dan §8.10 lolos. Tidak butuh admin kecuali disebut.

1. **Search via indeks.**
   1. Tunggu Settings → Indexer menampilkan Indeks "Siap".
   2. Di Ferrum, search `*.txt` dari `C:\`. Status bar harus menampilkan "via indeks", dan hasilnya keluar dalam ±1
      detik.
   3. Ulangi dengan pola regex atau filter isi. Kali ini harus memakai scan biasa (tanpa "via indeks").
2. **Disk Usage → Scan** pada `C:\Program Files`.
   1. Dialog harus menawarkan **Instan (indeks)** di samping Scan cepat (admin) dan Scan biasa.
   2. Pilih Instan. Hasil keluar tanpa UAC, dengan badge "Indeks".
   3. Bandingkan totalnya dengan Scan biasa. Selisih kecil wajar, karena file yang tidak bisa dibaca dan hardlink
      dihitung berbeda.
3. **Mode Bersama, dua akun.**
   1. Login sebagai user A. Buat `C:\Data\rahasia\x.txt`, lalu cabut hak list user B dari folder itu
      (`icacls C:\Data\rahasia /deny B:(RD)`).
   2. Login sebagai B dan search `x.txt` dari `C:\`. Hasilnya harus **kosong**. Disk Usage `C:\Data` dengan Instan
      harus menampilkan folder `rahasia` tanpa isi.
   3. Ubah ACL lagi (`icacls ... /remove:d B`). Dalam beberapa detik B bisa menemukan `x.txt` (cache dibuang lewat
      `SECURITY_CHANGE`).
4. **Profil sendiri di mode Bersama.** Search nama file yang ada di Desktop user dari `C:\`. File itu harus tetap
   ditemukan, dari walk profil sendiri yang digabung ("via indeks").
5. **Status jujur.** Lakukan hal berikut dari PowerShell elevated, lalu klik Periksa lagi di Settings setiap kali:
   - `sc.exe stop FerrumIndexer` → Settings: "Service mati — … scan biasa", dan search tetap bekerja (walker).
   - `sc.exe start FerrumIndexer` → selama build: "Sedang membangun indeks (C: nn%)", lalu "Siap".
6. **AI.**
   1. Di chat, tanya "indeksnya sudah siap?". Jawaban harus sesuai Settings.
   2. Minta "ganti indexer ke High performance". Kartu harus muncul dengan penjelasan privasi, dan tombol baru aktif
      setelah dicentang.
   3. Pilih Ganti mode, lalu UAC. Jawab No → "ditolak", mode tetap. Ulangi dan jawab Yes → mode berubah.
   4. Minta kembali ke Bersama. Kartu muncul, dan tidak ada UAC.
7. **AI copy threads.** Saat copy besar berjalan, minta "tambah thread copy". Kartu copy.adjust muncul, lalu
   Terapkan menaikkan jumlah thread.

### 9.8 FER-487: uji squatting manual (owner, butuh admin untuk langkah 1, 3, 9)

Tujuan: indeks tetap bisa dipakai saat user standar memegang nama tetap `\.\pipe\FerrumIndexer`.

1. (elevated) `sc.exe stop FerrumIndexer` dan tunggu sampai STOPPED.
2. (PowerShell biasa, sebagai user standar) squat nama tetap dan biarkan jendela ini terbuka:
   `powershell -NoProfile -Command "$p=New-Object System.IO.Pipes.NamedPipeServerStream('FerrumIndexer','InOut',1); Read-Host 'squat aktif'"`
3. (elevated) `sc.exe start FerrumIndexer`, lalu tunggu 5 detik.
4. (PowerShell biasa) `reg query HKLM\SYSTEM\CurrentControlSet\Services\FerrumIndexer\Parameters /v PipeName`.
   Nilainya harus `FerrumIndexer-` diikuti 32 digit hex.
5. (PowerShell biasa) `Get-Content C:\ProgramData\Ferrum\indexer.log -Tail 20` harus memuat
   `is held by another process` lalu `fallback name FerrumIndexer-… announced in PipeName`, dan
   `listening on \.\pipe\FerrumIndexer-<32 hex> (owner …)`.
6. (PowerShell biasa) `& "C:\Program Files\Ferrum\ferrum-indexer.exe" status` tanpa `pipe name taken`, dengan
   `degraded=false`. Ferrum → Settings → Indexer: Berjalan, dan search C: lewat indeks.
7. Tekan Enter di jendela squat untuk melepas nama. Ferrum harus tetap tersambung lewat nama cadangan.
8. (elevated) `scripts\indexer-verify.ps1`: semua PASS, termasuk "PipeName is absent or a valid fallback name" dan
   "PipeName key: owner Administrators, Users read only".
9. (elevated) `sc.exe stop` lalu `sc.exe start` dengan nama tetap bebas. `PipeName` harus sudah hilang (dihapus saat
   start), dan log tidak memuat `held by another process`.
10. Negatif (opsional, elevated): `reg add` value `PipeName` dengan nilai yang tidak sesuai format, misalnya
    `\.\pipe\x`. Ferrum tidak boleh memakai nilai itu; status kembali ke scan biasa. Hapus value itu setelahnya.

## 10. IPC v4: ukuran folder dan sync inkremental (S4, FER-477)

### 10.1 Protokol

`PROTO_VERSION = 4`. Versi tetap harus sama persis saat hello. App dan service selalu dipasang oleh satu
installer, jadi tidak ada negosiasi versi. App baru dengan service lama (atau sebaliknya) mendapat
`unsupported_version`, lalu Settings menampilkan `protocol`, semua fitur indeks memakai scan biasa, kolom ukuran
folder tetap "—", dan sync membaca seluruh isi file. Request maks **16 KiB** (naik dari 2 KiB karena
`folderSizes`; tetap dicek sebelum alokasi, 8 koneksi × 16 KiB), response tetap ≤ 1 MiB. Kedua request baru ikut
di-fuzz.

- **`folderSizes { parent, names[1..200] }`** → `FolderSizes { stale, sizes[] }`, satu per nama dengan urutan
  sama: `ok (bytes, files)` atau `not_indexed` (tidak ada, file/link, atau dikecualikan). Ukuran = jumlah ukuran
  logis semua file di bawah folder (rekursif; tiap nama hardlink dihitung). Bentuk `parent + names` (bukan 200
  path penuh) karena pemakainya selalu anak satu folder di file list.
  - `parent` lolos `parse_abs_dir`. Tiap nama lolos `valid_name`: ditolak `.`, `..`, `\`, `/`, `:` (alternate
    data stream), karakter kontrol, kosong, dan > 255 unit. Trailing byte ditolak.
  - **Mode Bersama: service menjawab `forbidden`** (keputusan desain 2026-10-11). Agregat ukuran bisa
    membocorkan ukuran isi folder yang tidak boleh dilihat klien, dan tidak ada AccessCheck per entri untuk
    agregat. Ditolak di service, bukan hanya disembunyikan di UI. Mode dibaca dari snapshot yang sama dengan daftar
    indeks yang menjawab (review M1).
  - Query berat (slot 2 global / 1 per user), lock baca per 32 768 slot, batas waktu 5 detik lalu `timeout`,
    dibatalkan saat mode berubah. `degraded` seperti query lain.
- **`changes { path, since: none | (journal_id, usn) }`** → `Changes { mark (journal_id, usn), status, dirs[] }`.
  - `status`: `complete` (dirs = semua folder di bawah `path` yang listing-nya berubah sejak `since`, relatif,
    `""` = folder itu sendiri), `mark_only` (tanpa `since`), atau `rescan:<alasan>`, yang berarti klien **wajib**
    scan penuh: `journal_changed` (journal lain), `unavailable` (riwayat sejak mark tidak lengkap: service restart,
    indeks dibangun ulang, journal wrap, log digusur, indeks stale atau tanpa journal), `too_many` (> 4096 folder
    atau ±512 KiB), `excluded_inside` (mode Bersama: profil/recycle bin di dalam path tidak diindeks, jadi
    perubahannya tidak tercatat).
  - Tiap folder membawa flag `recursive`: folder itu sendiri dibuat, di-rename, dipindah, dihapus, atau recordnya
    dipakai ulang, sehingga seluruh isinya dianggap berubah. Ini menutup kasus "folder lain bernama sama dipindah
    masuk".
  - Mark dengan `journal_id = 0` berarti volume tanpa journal (tidak ada mark).
  - **Barrier kesegaran.** Sebelum menjawab, service membangunkan worker lalu menunggu sampai indeks volume itu
    membaca journal sampai habis (`CatchUp::UpToDate`) dalam pass worker yang **dimulai setelah** request
    (`IndexShared::poll_gen`, `LiveIndex::caught_pass`). Jadi perubahan yang dibuat user sebelum menekan Sync
    selalu ikut. Batas tunggu 5 detik, lalu `timeout`. Service dijeda → `paused`. Worker yang sedang membangun
    volume lain (build MFT panjang) membuat barrier timeout → sync membaca seluruh isi.
  - Mode Bersama: `path` harus lolos `resolve_dir` (terlihat dan tidak dikecualikan; selain itu `not_indexed`
    seperti search). Folder yang dilaporkan hanya folder yang `children_visible`, yaitu folder itu dan semua
    leluhurnya bisa di-list klien (AccessCheck FER-476, tidak pernah di bawah lock indeks). Untuk non-admin, `usn`
    pada mark **dan** `since` yang dikirim klien dibulatkan ke bawah ke kelipatan 16 MiB (seperti I3), sehingga
    klien tidak bisa memetakan kapan perubahan user lain terjadi dengan mencoba `since` satu per satu. Pembulatan ke
    bawah selalu aman, karena jendela mulai lebih awal dan melaporkan lebih banyak, tidak pernah lebih sedikit.
  - Path changed directory yang lebih panjang dari 4096 byte, atau kandidat lebih dari 2 × batas jawaban, berarti
    `too_many` (rescan), tidak pernah dilewati diam-diam. Resolusi path di-memo per folder, jadi kerja satu query
    dibatasi jumlah folder unik, bukan entri log × kedalaman.

### 10.2 Change log di service (`changelog.rs`)

- Satu log per volume, di memori: maks **32 768 folder** (satu slot per folder berisi perubahan terakhirnya,
  ±80 byte per slot). Folder tertua digusur dulu, dan `floor` naik ke USN-nya + 1. Log hanya menjawab jendela
  `since ≥ floor`; selain itu `None` → `unavailable`. Log tidak pernah menjawab "tidak ada perubahan" untuk
  jendela yang tidak dilihatnya.
- `floor` awal = posisi journal saat indeks dibangun (diambil **sebelum** scan MFT; perubahan selama scan diputar
  ulang dan ikut tercatat) atau posisi file indeks yang dimuat. Log **tidak dipersist**: setelah service restart,
  sync pertama membaca seluruh isi sekali.
- Yang dicatat worker untuk setiap batch journal (`apply_changes_deferred`):
  - parent dari nama lama dan nama baru setiap record yang di-refresh;
  - parent yang disebut record journal itu sendiri (`ParentFileReferenceNumber`, `usn.rs`), yang menutup
    create+delete cepat dan record yang gagal dibaca;
  - folder yang identitas atau namanya berubah, dicatat `recursive`.

  Batch yang diulang (record gagal dibaca) tetap mencatat percobaan pertama, karena percobaan ulang tidak lagi
  melihat parent lama. Satu USN per batch (yang tertinggi), sehingga uji `usn ≥ since` tetap konservatif.

- Folder di log yang sudah tidak ada dilewati saat query. Penghapusannya tercatat di parent-nya yang masih hidup,
  dan parent itulah yang dilaporkan.

### 10.3 Ferrum

- **Kolom ukuran folder** (opsional, mati secara default): Settings → Indexer → "Tampilkan ukuran folder di kolom
  Size". `folderSizesStore` meminta folder yang sedang tampil, dikelompokkan per parent, dengan debounce 120 ms,
  cache 30 detik, dan jeda 60 detik setelah penolakan (`shared_mode`, `degraded`, service mati), supaya PC bersama
  tidak ditanya setiap scroll. Command `indexer_folder_sizes` memecah batch sesuai batas service (≤ 200 nama,
  frame ≤ 16 KiB, maks 1000 nama per panggilan) dan tidak pernah mengirim nama yang tidak valid. Path UNC tidak
  ditanyakan.
- **Sync inkremental — OPT-IN, mati secara default** (keputusan desain setelah review keamanan, §10.6).
  Settings → Indexer → "Sync cepat dengan journal indexer (lewati baca isi file yang terbukti tidak berubah)", pref
  `syncJournalSkip`. Pref dicek di frontend **dan** di command `compare_dirs` (Rust membaca pref dari disk); kalau
  mati, compare persis seperti sebelum FER-477 (tidak ada query ke service, tidak ada mark). Kalau nyala,
  `sync_incremental.rs`: compare rekursif by-content untuk sync (`compareForSync` mengirim `incremental: true`)
  membaca baseline pasangan root, lalu menanyakan `changes` untuk kiri dan kanan dengan mark
  dari baseline (satu koneksi, timeout 10 detik). Root dikanonikalkan (junction di-resolve). Root UNC atau volume
  tanpa huruf tidak ditanyakan. **Satu-satunya optimasi: baca isi sebuah pasangan file dilewati** bila semua
  syarat ini terpenuhi:
  1. kedua sisi `complete`;
  2. folder file itu tidak berubah di kedua sisi, dan tidak ada folder di atasnya yang `recursive`;
  3. fakta kiri dan kanan (size, mtime) **persis** sama dengan entri baseline.

  Listing direktori tetap dibaca dari disk. Status pasangan yang dilewati = status metadata, sama seperti baca isi
  yang menemukan byte sama. Masalah apa pun berarti compare biasa. `report.incremental` mencatat `used`, `reason`,
  dan `contentSkipped`.

- **Mark di baseline.** Mark diambil sebelum walk dan dikembalikan di `report.journal` (string desimal, karena u64
  tidak muat di number JS; divalidasi ketat di kedua sisi). `reconcileBaseline` hanya menyimpannya kalau settle
  compare **bersih total** (`provesFullSync`): rekursif, tidak truncated atau cancelled, tanpa error, semua entri
  `same`, dan setiap file tercatat dengan fakta yang baru diverifikasi. Mark tidak pernah diwarisi dari baseline
  lama. Run yang mengubah entri tanpa membuktikan sinkron penuh menyimpan baseline **tanpa** mark. Run yang tidak
  mengubah entri dan tidak membawa mark baru membiarkan file lama (mark lamanya tetap menanggal setiap entri, dan
  perubahan sejak mark itu ada di change list).
- Catatan desain: aturan "sync by content tidak percaya cache verdict" (`noCache`) tetap berlaku untuk cache di
  memori. Lompatan ini bukan cache: buktinya journal NTFS (setiap tulis data mencatat `DATA_*`/`CLOSE`) dan
  baseline yang diverifikasi by-content pada mark.
- **AI:** `indexer.status` menyebut ketersediaan ukuran folder (hanya High performance) dan sync inkremental
  (`features`). Kartu High performance menyebut bahwa kolom ukuran folder ikut aktif. `indexer.setMode` tetap
  hanya jalan setelah konfirmasi user di kartu (FER-476).

### 10.4 Batasan

- Ukuran folder dihitung dari indeks: file yang sedang ditulis bisa tertinggal ±1 detik, dan perpindahan folder di
  tengah query bisa membuat angkanya sementara kurang tepat (lock dilepas per potongan). Nilai ini untuk tampilan,
  bukan untuk keputusan.
- Mark untuk non-admin di mode Bersama dibulatkan 16 MiB. Pada volume yang sepi, dan karena floor hasil restart,
  jendela bisa jatuh di bawah floor sampai journal maju melewati batas 16 MiB berikutnya. Akibatnya sync membaca
  seluruh isi (benar, hanya tidak dipercepat).
- Change log di memori service ±2,6 MiB per volume pada kondisi terburuk, di luar budget indeks.
- **Celah sisa jendela journal (wrap, FER-503):** sesi tulis yang dibuka **sebelum record tertua journal** — journal
  sudah wrap melewatinya — dan masih terbuka saat indeks mulai tidak terdeteksi oleh siapa pun: pemindaian
  `prime_open_writes` (dan re-prime berkala FER-503) hanya melihat record yang masih ada di journal. Mitigasi yang
  sudah berlaku: mark baseline **kedaluwarsa setelah 7 hari** (run berikutnya memverifikasi seluruh isi lagi), dan
  sync cepat **default OFF**. Saran untuk pengguna: jangan mengandalkan sync cepat untuk folder berisi database,
  container, atau image VM yang handle tulisnya terbuka terus-menerus (berhari-hari) — untuk folder seperti itu
  verifikasi by-content tetap sumber kebenaran.
- **Status `open_unknown` dan pemulihannya (FER-503):** status ini aman (setiap `changes` menjawab rescan) dan
  muncul dari dua pemicu:
  - pemindaian sesi terbuka di awal (`prime_open_writes`) tidak selesai: service dijeda, journal > 512 MiB, atau
    journal tidak bisa dibaca;
  - lebih dari 4096 sesi terbuka, baik saat pemindaian awal **maupun saat indeks sedang mengikuti journal**. Sesi
    yang tidak muat dibuang dari pelacakan.

  Dulu status ini bertahan sampai indeks dibangun ulang. Sekarang worker mencoba ulang pemindaian (re-prime):
  percobaan pertama langsung di pass berikutnya setelah status muncul (`last_reprime` masih kosong), lalu paling
  cepat sekali per 10 menit (`OPEN_UNKNOWN_REPRIME`), dengan batas biaya yang sama (maks 8192 bacaan × 64 KiB).

- **Aturan penghapusan flag (review P2 FER-503).** Changelog mencatat USN terendah dari sesi yang tidak terlihat
  sejak flag di-set (`lowest_dropped_usn`): USN sesi yang dibuang karena batas 4096, atau USN tempat pemindaian awal
  berhenti. Re-prime yang selesai hanya menghapus flag kalau:
  - scan-nya mulai dari `first_usn` journal yang ≤ USN tersebut, sehingga record pembuka sesi itu ada di jendela scan
    dan sesinya sekarang terlacak; **dan**
  - tidak ada overflow baru sejak scan mulai (counter `open_overflows`), baik dari scan itu sendiri maupun dari follow
    journal.

  Kalau flag di-set tanpa USN yang diketahui (journal tidak bisa di-query saat pemindaian awal), scan lengkap apa pun
  dari record pertama journal boleh menghapusnya.

- **Kasus wrap: flag tetap aktif sampai rebuild.** Misalnya sesi dibuang karena batas saat indeks mengikuti journal,
  lalu journal wrap melewati record pembukanya sebelum re-prime. Tidak ada scan yang bisa menemukan sesi itu lagi,
  dan tulisan berikutnya lewat handle yang sama tidak membuat record baru. Re-prime tidak dijalankan
  (`ReprimeSkip::OutOfJournal`, dicatat di log service), dan `open_unknown` tetap aktif sampai indeks dibangun ulang
  atau dimuat ulang (restart service). Ini perilaku konservatif seperti sebelum FER-503. Non-admin bisa memaksa
  keadaan ini (buka > 4096 handle tulis, buat churn sampai journal wrap); akibatnya hanya sync cepat volume itu mati
  sampai restart, bukan jawaban yang salah.
- **Replay re-prime terpisah dari follow (review 2 FER-503).** Karena re-prime berjalan lintas pass, follow journal
  tetap memperbarui sesi terbuka di sela-selanya. Karena itu replay tidak menyentuh `open` secara langsung:
  - replay (record di bawah target, yaitu posisi indeks saat scan mulai) dikumpulkan di map terpisah (`Replay`);
  - selama scan berjalan, follow mencatat record yang `CLOSE`-nya ia lihat (semuanya pada USN ≥ target, maks 65 536
    record);
  - saat scan selesai (`ChangeLog::finish_reprime`), sesi hasil replay yang masih terbuka ditambahkan ke `open`,
    **kecuali** follow sudah melihat `CLOSE`-nya. `CLOSE` lama hasil replay tidak pernah menghapus sesi yang dicatat
    follow, misalnya file yang ditutup lalu dibuka lagi selama scan;
  - kalau hasil gabungan atau replay menembus cap 4096, kejadiannya dihitung overflow: flag tetap aktif dan
    `lowest_dropped_usn` diperbarui. Kalau follow melihat lebih dari 65 536 `CLOSE` selama scan, tidak ada yang
    digabung dan flag tetap aktif sampai percobaan berikutnya.
- Dengan aturan di atas, log tidak pernah menjawab "tidak ada perubahan" untuk sesi yang **sempat terlihat** atau
  **dibuang** oleh indeks ini. Yang tetap tidak terdeteksi hanya celah sisa di poin "Celah sisa jendela journal":
  sesi yang record-nya sudah keluar dari journal **sebelum indeks mulai**. Entri sesi yang terlacak tetap dilaporkan
  berubah sampai `CLOSE`-nya terlihat.
- **Biaya re-prime.** Satu worker melayani semua volume. Re-prime dipecah menjadi maks 256 bacaan (16 MiB) per pass
  (`REPRIME_ROUNDS_PER_STEP`), lalu dilanjutkan di pass berikutnya, sehingga volume lain tetap mengikuti journal di
  sela-selanya. Scan 512 MiB penuh butuh ±32 pass. Replay tidak memegang write lock per batch (hanya read lock
  singkat untuk memeriksa journal ID); write lock dipegang sekali saat scan mulai dan sekali saat penggabungan. Batasan
  yang tersisa: pass yang memuat potongan re-prime menjadi lebih lama (hingga 16 MiB baca journal), sehingga barrier
  `changes` di volume lain bisa menunggu sedikit lebih lama. Non-admin yang menahan > 4096 handle tulis bisa memicu
  ini tiap 10 menit. Dampaknya terbatas dan tetap fail-safe.

### 10.5 Verifikasi manual (owner, service terpasang)

1. Pasang build ini. Settings → Indexer: Berjalan, mode **High performance**, indeks Siap.
2. Nyalakan "Tampilkan ukuran folder di kolom Size". Buka `C:\Program Files`. Baris folder harus menampilkan ukuran
   dalam ±1 detik. Bandingkan satu folder dengan Properties di Explorer; selisih kecil wajar karena hardlink dan
   file yang sedang ditulis.
3. Ganti ke **Bersama** (dialog → Ganti mode). Kolom ukuran folder harus kembali "—", dan Settings menampilkan
   catatan mode Bersama. PowerShell biasa (opsional):
   `& "C:\Program Files\Ferrum\ferrum-indexer.exe" status` → `mode=shared`.
4. Kembali ke High performance (UAC). Ukuran muncul lagi.
5. **Sync inkremental** (nyalakan dulu Settings → Indexer → "Sync cepat dengan journal indexer"; pastikan dengan
   pengaturan mati panel Sync tidak menampilkan baris "Sync cepat"). Siapkan dua folder di C: (misalnya
   `C:\SyncTest\L` dan `C:\SyncTest\R`) dengan beberapa ratus MB file. Sinkronkan dua arah sampai selesai, sehingga baseline tersimpan dengan mark.
   1. Sync lagi tanpa perubahan. Durasi settle harus jauh lebih cepat dari run pertama (tidak ada baca isi). Log
      Ferrum tidak menyebut `incremental sync not used`.
   2. Ubah isi satu file di `L` **tanpa** mengubah ukuran, lalu kembalikan mtime-nya (PowerShell:
      `$f=Get-Item C:\SyncTest\L\x.bin; $t=$f.LastWriteTime; <ubah byte>; $f.LastWriteTime=$t`). Sync → file itu
      harus terdeteksi berbeda (folder-nya tercatat berubah di journal).
   3. Restart service (admin: `sc.exe stop FerrumIndexer` lalu `start`). Sync pertama setelahnya membaca seluruh
      isi (`unavailable`), dan sync berikutnya cepat lagi.
   4. Mode Bersama: ulangi langkah 1 untuk folder di luar profil, misalnya `C:\SyncTest`. Hasilnya harus tetap
      cepat. Folder di dalam profil sendiri (`C:\Users\<kamu>\…`) tidak diindeks di mode Bersama, jadi sync-nya
      membaca seluruh isi. Ini disengaja.
6. **Barrier:** sambil sync berjalan, `sc.exe pause FerrumIndexer` (admin). Compare sync berikutnya harus tetap
   benar (baca seluruh isi, `service_unavailable`/`paused`). Lalu `sc.exe continue FerrumIndexer`.
7. AI: tanya "indeksnya siap?". Di High performance jawabannya menyebut ukuran folder tersedia; di Bersama menyebut
   tidak tersedia.

### 10.6 Review keamanan FER-477: perbaikan P2 dan opt-in

Review keamanan menyatakan sync inkremental tidak lulus kalau aktif secara default. Keputusan
desain: perbaiki keempat temuan P2 **dan** jadikan fiturnya opt-in.

- **P2-1, mark di depan journal.** Kalau `since.usn` lebih besar dari posisi indeks (volume di-restore dari image
  atau snapshot VM: journal ID sama, USN mundur), jawabannya `rescan:unavailable`, bukan `complete` kosong.
- **P2-2, root atau leluhurnya diganti.**
  - `changes` memeriksa flag `recursive` sejak mark untuk folder yang diminta **dan setiap leluhurnya** sampai root
    volume. Kalau ada yang diganti, jawabannya `rescan:root_changed`.
  - Jawaban membawa `root_frn` (file reference folder yang diminta, termasuk sequence). Klien menyimpan
    `rootFrn` dan path kanonis (`X:\a\b`, junction di-resolve) di mark.
  - Pada run berikutnya, path kanonis lain (junction di-retarget) atau `root_frn` lain berarti `root_changed`, dan
    tidak ada lompatan. Jawaban `complete` untuk journal ID lain dari yang ditanyakan juga ditolak klien.
- **P2-3, file yang ditulis lewat handle yang tetap terbuka.** NTFS hanya menulis record USN saat sebuah flag reason
  baru muncul, sehingga tulisan berikutnya lewat handle yang sama tidak tercatat sampai `CLOSE`.
  - **Pilihan: parent dari setiap sesi tulis yang masih terbuka selalu dilaporkan berubah**, di jendela mana pun,
    sampai `CLOSE`-nya datang. Sesi terbuka = ada reason data (`DATA_*`/`NAMED_DATA_*`) tanpa `CLOSE`. Parent
    diambil dari record journal dan dari nama record saat ini.
  - Alternatif `mark = min(next_usn, USN pertama sesi terbuka)` tidak dipilih. Mark seperti itu bisa tertinggal di
    bawah floor log selama sesi terbuka lama (database, container), sehingga fitur mati terus-menerus. Selain itu
    mark itu tetap tidak menutup sesi yang dimulai sebelum indeks.
  - **Sesi yang dimulai sebelum indeks** ditemukan dengan memindai journal dari record pertamanya sampai posisi
    indeks setiap kali indeks dibangun atau dimuat (`engine::prime_open_writes`, maks 8192 bacaan × 64 KiB).
  - Kalau pemindaian itu tidak selesai, atau sesi terbuka lebih dari 4096 (saat pemindaian atau saat mengikuti
    journal), log menjadi `open_unknown` dan setiap jawaban `rescan:unavailable`. FER-503: worker mencoba ulang
    pemindaian itu pada indeks yang hidup (pertama langsung, lalu paling cepat sekali per 10 menit). Scan yang selesai
    hanya memulihkan status kalau tidak meluber dan masih menjangkau setiap sesi yang sempat dibuang; kalau sesi itu
    sudah keluar dari journal, status bertahan sampai rebuild atau reload (§10.4).
  - Sisa yang tidak tertutup: sesi yang record tulis pertamanya sudah keluar dari journal (handle terbuka lebih lama
    dari retensi journal dan tidak pernah ditutup).
- **P2-4, AccessCheck yang tidak bisa dievaluasi.** `DirAccess::check` punya tiga keadaan: `Yes`, `No`, `Unknown`.
  - `Unknown` berarti service tidak bisa membuka folder lewat file ID, tidak bisa membaca SD-nya (misalnya DACL tanpa
    SYSTEM), atau AccessCheck sendiri gagal.
  - Search dan subtree tetap memperlakukan `Unknown` sebagai "tidak" (privasi, fail closed).
  - `changes` menjawab `rescan:unverifiable`; folder seperti itu tidak pernah dilewati diam-diam.
  - Cache AccessCheck tidak menyimpan `Unknown`. Untuk `changes`, `false` di cache tidak dipercaya sebagai "tidak"
    yang pasti, jadi dievaluasi ulang. FER-503: cache kini hanya menyimpan hasil pasti (`Yes`/`No`; `Unknown` tetap
    tidak pernah disimpan), sehingga `No` yang pasti **dipakai ulang** — folder yang dilarang tidak lagi
    dievaluasi ulang di setiap query `changes` (dulu ±32 ribu kali + 4096 sesi per query, beban yang bisa dipicu
    non-admin berulang kali).
- **P3 (dikerjakan):** batas jawaban (`too_many`: > 4096 folder atau > 512 KiB) dihitung **setelah** filter
  visibilitas mode Bersama, jadi tidak ada oracle "banyak folder berubah di area yang tak terlihat".
- **Opt-in dan batas waktu.**
  - Pengaturan "Sync cepat dengan journal indexer" mati secara default. Panel Sync menampilkan jumlah file yang
    isinya dilewati (preview, dan verifikasi setelah sync), atau alasan fitur tidak dipakai.
  - Mark membawa `takenAtMs` dan **kedaluwarsa setelah 7 hari** (juga kalau tanggalnya > 1 hari di masa depan).
    Setelah kedaluwarsa, run berikutnya membaca semua isi lalu mengambil mark baru.
- **Risiko sisa (fitur opt-in, dijelaskan di Settings):** perubahan yang tidak tercatat journal tidak terdeteksi
  sampai verifikasi penuh berikutnya (paling lama 7 hari). Contohnya penulisan dari OS lain (dual-boot, ntfs-3g),
  alat yang menulis langsung ke volume, dan bit-rot.
- **Follow-up (P3 review):**
  - ~~barrier menunggu sampai 5 detik sebelum slot query berat diambil, dan setiap request mem-kick worker (minimal
    200 ms antar pass)~~ — **FER-503**: batas tunggu barrier kini **satu request per identitas**; request lain dari
    identitas yang sama dijawab `busy` seketika (kode wire yang sudah ada; run itu membandingkan by-content). Kick
    per request tidak diubah.
  - ~~`folderSizes` memindai indeks dua kali per request dengan memo yang bisa besar (hanya mode High performance,
    dibatasi slot berat)~~ — **FER-503: diperiksa, TIDAK digabung.** Pass 1 (temukan child yang diminta) harus
    selesai sebelum pass 2 (atribusi file) karena target baru bisa ditemukan di tengah scan; menggabungkannya
    menuntut penjualan per-parent lalu penelusuran naik (memo, lokalisasi lebih buruk) untuk hasil yang sama.
    Dua scan berurutan yang hemat cache tetap lebih murah dan lebih sederhana; tetap jadi usulan bila terukur
    mahal di lapangan.
  - floor log yang naik karena eviction masih bisa menjadi sinyal kasar di mode Bersama;
  - opsi "verifikasi penuh" per run.
