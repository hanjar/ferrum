# RAR Fixtures (Sintetis saat Tes)

Berkas arsip `.rar` **tidak di-commit ke repositori** karena alasan lisensi WinRAR. Ferrum tidak menyertakan maupun mewajibkan WinRAR.

## Mekanisme Pembuatan Fixture

Seluruh fixture RAR dibuat secara dinamis saat tes berjalan (`archive_rar.rs`, `archive_create_test.rs`, `archive_member_keys.rs`) menggunakan `Rar.exe` milik penguji sendiri:

1. **Pencarian `Rar.exe`:**
   - Variabel lingkungan `FERRUM_RAR_EXE` (jika disetel, harus menunjuk ke `Rar.exe` yang valid; jika tidak valid, pencarian berhenti dan tes di-skip).
   - `C:\Program Files\WinRAR\Rar.exe`
   - `C:\Program Files (x86)\WinRAR\Rar.exe`
   - `PATH`
2. **Jika `Rar.exe` tidak ditemukan:** Tes-tes yang membutuhkan fixture RAR akan di-skip secara otomatis dengan pesan yang jelas (misalnya `SKIP: Rar.exe not found`). Pengujian tidak gagal.
3. **Jika `Rar.exe` ditemukan:** Fixture sintetis dibuat di direktori sementara unik (`ferrum-rartest-*`) sekali per proses tes (`OnceLock`) dari payload data sintetis (struktur bertingkat, Unicode, data pseudo-acak) dan dibersihkan setelah selesai.

## Password Uji
- Password: `FerrumTest123` (konstanta publik untuk tes otomatis, bukan rahasia).
