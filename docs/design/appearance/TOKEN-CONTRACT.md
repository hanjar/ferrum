# TOKEN-CONTRACT — Ferrum Appearance (APP-00, FER-371)

Status: **BEKU** untuk gelombang implementasi APP-01/02/03/06/07/08. Sumber nilai: `audit/v3-token-extract.md` (mock v3, dibaca langsung; SHA256 PENDING-SHELL). Keputusan menyimpang dari mock diberi alasan eksplisit. Saat worktree siap, dokumen ini pindah ke `ferrum/docs/design/appearance/TOKEN-CONTRACT.md` + entri keputusan D50+ di `docs/decisions.md`.

## 1. Registry ID (stabil — jangan diubah lagi)

| ID         | Label UI    | Chip dot (accent dark) |
| ---------- | ----------- | ---------------------- |
| `violet`   | Pop Violet  | #7B3FFF                |
| `electric` | Electric    | #2563FF                |
| `aqua`     | Aqua Signal | #0096E6                |
| `green`    | Fresh Mint  | #00B86B                |
| `yellow`   | Sunshine    | #FFC400                |
| `orange`   | Tangerine   | #FF6A00                |

- Mode TERPISAH dari palet: `theme: 'dark'|'light'|'system'`; dokumen selalu menerima `data-theme` EFEKTIF (`dark|light`) + `data-palette`.
- Green variant emerald/lime **tidak ada** fase ini (keputusan handover; default v3 = blok green standar).

## 2. Atribut dokumen (satu titik kebenaran: `ui.applyToDocument()`)

| atribut        | nilai                   | sumber state                |
| -------------- | ----------------------- | --------------------------- |
| `data-theme`   | `dark\|light` (efektif) | `ui.theme` + OS bila system |
| `data-palette` | 6 ID                    | `ui.palette`                |
| `data-density` | `compact\|comfortable`  | `ui.density`                |
| `data-icon`    | `colored\|monochrome`   | `ui.iconStyle`              |
| `data-ui-font` | `12\|13\|14`            | `ui.uiFontSize`             |

## 3. Token CSS

### 3a. Token struktural BARU di `:root` (dari v3 §2)

`--onAccent:#fff`, `--side:var(--titlebar)`, `--flash`, `--greenInk:var(--green)`, `--amberInk:var(--amber)`, `--redInk:var(--red)`, `--greenSoft`, `--amberSoft`, `--redSoft`, `--pop`, `--popInk`, `--popOn`, `--popSoft`, `--duoGrad`, `--accentGrad` (default = violet).

### 3b. Koreksi mix v2→v3 (WAJIB, bukan opsional)

- dark: `--sel` = accent **40%**, `--accentSoft` = **22%**; light: **26%** / **14%**.
- Light palette updates: violet `--text3:#8A8699`, `--green:#00B85C`, `--red:#FF2D55`; electric `--green:#00B34A`; aqua `--green:#5CB800`. Nilai light lama yang bergeser (mis. `#0E9F4E`) TETAP hidup sebagai `--greenInk` (untuk teks/ikon di atas latar terang). Tabel nilai penuh 12 kombinasi = `audit/v3-token-extract.md` §3 (tidak diduplikasi di sini).
- `--side` light per palet: violet #F6F2FF, electric #F1F5FF, aqua #EEF8FB, green #F8FAF8, yellow #FAFAF6, orange #FBF9F8.

### 3c. File-type `--ft*` (keputusan D-dev1)

- Menjadi **per palet** (dark): nilai dari tabel §4 ekstraksi (violet #3B82F6/#00D1C1/#FF4FA3/#FFC928/#2ED67A/#FF6B3D dst; kategori `pdf` ditambah ke set token sebagai `--ftPdf`).
- Light: SATU set override global light dipertahankan dari app.css v2 (kontras di latar putih). **Deviasi dari mock** — mock tidak membedakan mode untuk warna file; alasan kontras/WCAG (target APP-05 ≥3:1 elemen UI).
- Kategori tanpa warna (config/unknown) = `#7D8AA8` (v3).

### 3d. Density & font (mechanism ditetapkan kontrak — mock hanya acuan)

- `html[data-density='comfortable'] { --row: 30px; }` — default `--row:26px` (compact) tetap di `:root`. Acuan v3 26/30 px pada font 13.
- `html[data-ui-font='12'] { --uiFont:12px }` / `'14'` → 14px; default `--uiFont:13px`. `body`/`.shell` memakai `var(--uiFont, 13px)`.
- Row minimum: numeric itemSize virtualizer (APP-06) WAJIB dihitung `max(--row, ceil(fontSize * 1.85))` — font 14 → min ≥26px; font besar tidak memotong. Sumber angka = CSS var, bukan hardcode kedua.

### 3e. Monochrome icons (kontrak kecil, konsumen APP-07)

`html[data-icon='monochrome']` → warna glyph file/folder = `var(--text3)`. Hanya glyph file/folder; overlay/badge status, selection, warning/error, dan treemap TIDAK ikut ternetralkan. Bentuk icon tidak berubah.

## 4. Preferensi persisten (kontrak APP-03)

Schema `config.json` (serde camelCase, container `#[serde(default)]` — field hilang = default; **migrasi aditif tanpa versi schema baru**):

| field TS       | field Rust      | tipe   | default   | valid                     |
| -------------- | --------------- | ------ | --------- | ------------------------- |
| theme          | theme           | string | `dark`    | dark/light/system         |
| palette        | palette         | string | `violet`  | 6 ID (3 lama tetap valid) |
| showHidden     | show_hidden     | bool   | false     | —                         |
| terminal       | terminal        | string | `auto`    | auto/wt/cmd/powershell    |
| density        | density         | string | `compact` | compact/comfortable       |
| iconStyle      | icon_style      | string | `colored` | colored/monochrome        |
| uiFontSize     | ui_font_size    | number | 13        | 12/13/14                  |
| showExtensions | show_extensions | bool   | **true**  | —                         |
| foldersFirst   | folders_first   | bool   | **true**  | —                         |

- Default dipilih = **perilaku existing** (D03 dark/violet, D13 folders-first, extensions tampil penuh, row 26, font 13) → pengguna lama tidak melihat perubahan.
- Validasi field-wise di load & save (pola `prefs.rs` existing); nilai rusak → default field itu saja, field lain selamat.
- Save = snapshot penuh semua field (debounce 300 ms, coalesce) → rapid switching tidak lost-update; kegagalan save → `prefs.saveError = true` (terlihat UI, retry di perubahan berikutnya, tidak ada klaim "tersimpan").
- Startup: `prefs.init()` apply sebelum interaksi (sudah ada); `?theme=` override tetap terakhir (hook test).

## 5. Semantik kontrol (kontrak APP-02/08)

- `showExtensions`: HANYA format label tampilan (pola v3 baris 1670): strip suffix terakhir; directories, `..`, dotfile, dan nama tanpa suffix utuh; rename/tooltip/properties/path operasi = nama asli. Multi-extension `.jsonl.gz` → tampil `x.jsonl` (terdokumentasi).
- `foldersFirst`: filter/comparator memisahkan dir SEBELUM arah sort (folder di atas meski descending, pola v3 §6); `false` = murni comparator kolom; `..` selalu posisi khusus terdepan; TIDAK menyentuh ordering Disk Usage (largest-first kontrak DSK).
- `showHidden`: state tunggal existing (D08); sumber kebenaran policy = provider/backend (dotfile Unix + atribut hidden/system Windows), bukan cek titik frontend. Shortcut/menu existing di-reuse & sinkron (APP-08).
- System theme: `matchMedia('(prefers-color-scheme: light)')` listener live; cleanup saat dispose; nilai tersimpan TETAP `system`.

## 6. Acceptance APP-00 yang sudah/belum terpenuhi

- ✅ Tabel token lengkap 12 kombinasi (audit/v3-token-extract.md §3), ID palet stabil (§1), keputusan fallback/persistence (§4), mapping token lama (§3a–3c), provenance (SHA256 v3 `16585eff497ae56c1af7655e1f92cdb1d7e2b4a14ca1f505a6fcc1ef9f49a3e7`).
- ✅ Base SHA worktree `feature/appearance` = `3bc121149aaf97bf4466d9ae7cee5a9da94263ab` (main `3bc1211`, bersih) — dibuat 2026-09-30. SHA kontrak untuk lane Disk Usage = SHA256 dokumen ini di commit pertama branch.

---

## REVISI 1 — 2026-09-30 (disetujui pengguna): font level S/M/L/XL menggantikan 12/13/14

§3d (font) DIGANTI: Interface font size adalah **level bernama** — Small/Medium/Large/Extra Large — diimplementasikan sebagai offset global `--uiStep` (S −1px, M 0px, L +2px, XL +4px) yang dijumlahkan ke basis px masing-masing aturan: `font-size: calc(13px + var(--uiStep, 0px))`. Atribut dokumen `data-ui="s|m|l|xl"`. Setiap bagian mempertahankan basis dan hierarkinya; level menaikkan/menurunkan SEMUA bagian satu tingkat.

**Kopling geometri (WAJIB):** `rowPx = max(density 26/30, ceil((13 + step) × 1.85))` — M 26, L 28, XL 32 (compact); virtualizer membaca rowPx yang sama.

**Preview content dikecualikan** (container preview menetapkan `--uiStep: 0px` lokal).

§4 (prefs): field `uiFontSize: 12|13|14` DIGANTI `uiFontLevel: 's'|'m'|'l'|'xl'` (default `m`). **Migrasi**: config lama dengan `uiFontSize` angka dipetakan 12→s, 13→m, 14→l saat load; field lama tidak ditulis lagi. Deviasi dari mockup v3 (dropdown "13 px") tercatat di decisions.md D50.
