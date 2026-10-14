# Copy multithread adaptif: sebelum dan sesudah (FER-471)

Hanya membandingkan engine Ferrum dengan dirinya sendiri (tanpa robocopy, sesuai keputusan 2026-10-07).

## Cara menjalankan

```powershell
cd src-tauri
$env:CARGO_BUILD_JOBS=4
cargo build --release --example bench_copy_adaptive
.\target\release\examples\bench_copy_adaptive.exe auto      # adaptif (default)
.\target\release\examples\bench_copy_adaptive.exe 8         # tetap 8 thread
.\target\release\examples\bench_copy_adaptive.exe 1         # tetap 1 thread
.\target\release\examples\bench_copy_adaptive.exe stream1   # bentuk lama: 1 worker, salin buffer + flush per file
```

Dataset dibuat di `%TEMP%` lalu dihapus: **small-20k** (20.000 file 0,5-47 KB, 469 MB, 40 folder), **large-20**
(20 file x 32 MB, 640 MB), **mixed** (5.000 file kecil + 5 file x 64 MB, 437 MB). Sumber dan tujuan di `C:` (SSD menurut IOCTL seek penalty,
sama-sama satu volume), Windows Defender aktif.

## Hasil (2026-10-07, PC pengembangan, build release)

"Sebelum" = engine di `main` @ `ac0bedd` (satu file sekali waktu, loop baca/tulis 1 MiB + `sync_all` per file), dua
kali jalan. "Sesudah" = branch `feature/fer-471-copy-multithread`.

| Dataset   | Sebelum (2 run)                  | Sesudah: auto             | 8 thread              | 1 thread (CopyFile2) |
| --------- | -------------------------------- | ------------------------- | --------------------- | -------------------- |
| small-20k | 53,3 s / 57,5 s (375/348 file/s) | **17,9 s** (1.120 file/s) | 18,5 s (1.081 file/s) | 26,6 s (752 file/s)  |
| large-20  | 0,68 s / 0,77 s (945/831 MB/s)   | **0,19 s** (3.449 MB/s)   | 0,23 s (2.819 MB/s)   | 0,31 s (2.086 MB/s)  |
| mixed     | 12,4 s / 12,6 s (402/396 file/s) | **4,17 s** (1.199 file/s) | 5,02 s (997 file/s)   | 6,90 s (725 file/s)  |

- Speedup auto vs sebelum: small-20k **3,1x** (rata-rata 55,4 s menjadi 17,9 s), mixed **3,0x**, large-20 **3,8x**.
- Sumbernya dua hal terpisah: (1) CopyFile2 tanpa `FlushFileBuffers` per file (kolom 1 thread sudah ±2x lebih cepat
  dari sebelum), (2) pool paralel (auto ±1,5x di atas 1 thread untuk file kecil).
- **Catatan large-20:** file 32 MB baru ditulis oleh generator dan masih di page cache, jadi angkanya mengukur overhead
  engine, bukan kecepatan disk. Jangan dibaca sebagai throughput disk. Untuk satu file besar yang benar-benar dibaca
  dari disk, batasnya tetap kecepatan disk.
- Mode auto di SSD satu volume mulai di 4 worker dan naik sampai batas 8 (satu volume = batas lebih rendah).
  8 thread tetap vs auto selisihnya masih dalam derau (Defender dan NTFS membatasi ±1.100 file/s di PC ini).
- Smoke jendela nyata (`scripts/smoke-copy-threads.mjs`): 24.000 file + 4 x 48 MB (±520 MB) selesai dalam 22 s
  termasuk klik +/- dan ganti prioritas selama berjalan.

## Perubahan perilaku yang disengaja

- Copy biasa (F5) tidak memanggil `FlushFileBuffers` per file lagi (robocopy dan Explorer juga tidak); penulisan tetap
  lewat file staging dan rename atomik, jadi tujuan tidak pernah berisi file setengah jadi, hanya durabilitas bila
  listrik mati beberapa detik setelah selesai yang berkurang (sumber masih utuh). Settings "Tulis aman (flush) untuk
  copy" (default mati) mengembalikan flush per file.
- **Move selalu durable** (`CopyOptions.durable=true`, semua jalur `copy_verify_delete`): tiap file staged di-flush
  sebelum dipublikasikan dan sumber baru dihapus sesudahnya, sama dengan engine lama. Angka di atas untuk copy biasa;
  move lintas volume memakai flush sehingga lebih lambat.
- Nama tujuan: kunci klaim case-insensitive dan mencakup semua kandidat bernomor; tujuan baru atau hasil rename
  dipublikasikan dengan rename tanpa replace (tabrakan = galat per file). Overwrite untuk tujuan yang sama diserialisasi
  dalam urutan sumber (sumber terakhir menang).
- Salinan stream membandingkan ukuran+mtime sumber sebelum/sesudah; berubah = item gagal, sumber tidak dihapus oleh move.
- `CopyFile2` membawa atribut dan semua timestamp sumber (termasuk read-only). Sebelumnya hanya mtime yang dibawa.
- Sumber yang sedang ditulis proses lain (log aktif) gagal di `CopyFile2` dengan sharing violation; engine otomatis
  jatuh ke salin stream yang berbagi semua akses (ada tesnya).

## Batasan pengukuran

- Satu PC, satu volume SSD; belum ada angka HDD, USB, atau share SMB (jalur Network/HDD hanya diuji lewat tes unit
  profil media, bukan perangkat nyata).
- Auto bergantung pada deteksi media (`IOCTL_STORAGE_QUERY_PROPERTY`, tanpa admin). Bila gagal, media `Unknown`:
  mulai 2 worker, batas 8.

## Ukur ulang setelah perbaikan review (durable, klaim nama, commit tanpa replace)

Copy biasa, auto: small-20k 16,4 s (1.217 file/s), mixed 4,03 s, large-20 0,18 s; 1 thread: 25,6 s / 7,5 s / 0,24 s.
Tidak ada penurunan berarti dibanding tabel di atas.
