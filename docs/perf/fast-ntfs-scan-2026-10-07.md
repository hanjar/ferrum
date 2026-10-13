# Disk usage: scan cepat NTFS (MFT) vs scan biasa (FER-460)

Membandingkan engine `ntfsMft` (helper admin membaca MFT, lihat `docs/disk-usage-contract.md` §7) dengan walker
pool DSK-09 (scan biasa) di volume nyata.

## Cara menjalankan

Bagian MFT butuh terminal **elevated** karena membaca volume mentah. Jalankan sendiri, jangan dari agent tanpa
pengawasan, dan jangan bersamaan dengan build lain:

```powershell
cd src-tauri
$env:CARGO_BUILD_JOBS=4
cargo run --release --example bench_fast_scan -- C
```

Harness memanggil scan MFT in-process milik helper (parser dan akses read-only yang sama, tanpa pipe), membangun
snapshot Disk usage persis seperti di app, lalu menjalankan walker biasa atas `C:\`. Urutannya menguntungkan walker
karena walker jalan kedua, dengan cache metadata yang sudah hangat.

Angka end-to-end di app (termasuk UAC dan transfer pipe) bisa dibaca dari `durationMs` pada event `done` (log devtools),
atau diukur manual dengan stopwatch dari klik "Scan cepat" sampai treemap tampil.

## Hasil

Diukur owner pada 2026-10-07 di branch `feature/fer-460-fast-ntfs-scan` @ `e8e135a`, build release, volume `C:\`
(NTFS, ±4,9 juta record MFT). Kedua engine dijalankan terpisah: walker dari terminal biasa, MFT dari terminal
elevated (`-- C mft-only`).

| Engine                    | Durasi      | File      | Folder  | Logical             | Catatan                                    |
| ------------------------- | ----------- | --------- | ------- | ------------------- | ------------------------------------------ |
| MFT: baca + parse         | 9,22 s      |           |         |                     | 4.891.904 record, 3.838.634 entri, 0 rusak |
| MFT: bangun tree/snapshot | 1,76 s      |           |         |                     | `disk_usage::fast::build_outcome`          |
| **MFT total**             | **10,98 s** | 3.308.059 | 530.061 | 1.776.407.129.740 B | coverage `complete`                        |
| Walker DSK-09 (non-admin) | 62,58 s     | 3.241.910 | 515.154 | 1.754.479.414.858 B | coverage `partial`, 614 error (ACL)        |

**Speedup: ±5,7×** (62,6 s → 11,0 s) untuk seluruh C:.

Selisih hitungan (MFT +66.149 file, +14.907 folder, +21,9 GB) sesuai dengan kontrak §7.2. Walker tidak bisa masuk ke
614 path yang ditolak ACL (mis. `System Volume Information`, profil/`$Recycle.Bin` milik akun lain), sedangkan helper
membaca MFT sebagai admin dan menghitungnya. Hardlink dihitung per nama di kedua engine.

Belum termasuk di angka MFT: waktu menjawab prompt UAC dan transfer entri lewat pipe ke Ferrum. Transfer berjalan
paralel dengan pembacaan, karena helper mengirim batch sambil membaca. Uji end-to-end di app tercatat terpisah (§7.5
kontrak).

Selisih jumlah file antara kedua engine wajar: walker tidak bisa masuk ke folder yang ditolak ACL dan mencatatnya
sebagai error, sedangkan MFT membacanya sebagai admin (kontrak §7.2).
