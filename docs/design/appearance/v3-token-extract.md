# Ekstraksi token v3 — prototipe desain Ferrum File Manager v3

Provenance:

- Path: `mockups/Ferrum File Manager v3.dc.html`
- Tipe: mockup Claude Design single-file (helmet `<style>` di baris 14–45, markup 47–~1050, DCLogic JS ~1050–2094; total 2094 baris).
- SHA256: `16585eff497ae56c1af7655e1f92cdb1d7e2b4a14ca1f505a6fcc1ef9f49a3e7` (dihitung 2026-09-30, shell aktif).
- Sumber nilai: SEMUA nilai di bawah dibaca langsung dari file (baris dicantumkan). Tidak ada nilai yang ditebak.

## 1. Mekanisme selector (baris 15–33)

- Dark: `:root` + `html[data-palette="X"]`; Light: `html[data-palette="X"][data-theme="light"]`.
- Default dark = blok `:root` (violet). `data-theme` dan `data-palette` di-set pada `documentElement` (baris 941–944 `applyVars()`).
- Green punya varian `[data-green="emerald"|"lime"]` (baris 30–33) — **TIDAK diadopsi fase ini** sesuai kontrak produk; default green = blok `html[data-palette="green"]` (baris 24–25).
- Density: `--row` di-set inline pada documentElement: comfortable = `30px`, compact = `26px` (baris 946).
- `--r`/`--rl` per platform windows/linux/macos (baris 947–948) — app sudah default Windows (4px/6px), tidak berubah.

## 2. Token struktural `:root` (baris 15) — DELTA vs app.css v2

Token baru yang BELUM ada di `ferrum/src/app.css`:
`--onAccent:#fff`, `--side:var(--titlebar)`, `--flash:color-mix(in oklch,var(--green) 24%,transparent)`,
`--greenInk:var(--green)`, `--amberInk:var(--amber)`, `--redInk:var(--red)`,
`--greenSoft:color-mix(in oklch,var(--green) 16%,transparent)`, `--amberSoft:…16%…`, `--redSoft:…14%…`,
`--pop`, `--popInk`, `--popOn`, `--popSoft` (per palet), `--duoGrad`, `--accentGrad` (per palet).

Perubahan mix (v2 app → v3):

- dark: `--sel` accent 30% → **40%**; `--accentSoft` 16% → **22%**.
- light: `--sel` 18% → **26%**; `--accentSoft` 11% → **14%**.
- light `--folder`: `var(--accent)` (sama seperti v2).

Light struktural (baris 16) identik v2 (titlebar, hover, line, kbd, scrim, shadow, tabShadow, selDim, bar) — tidak ada delta.

## 3. Palet × mode (baris 17–29) — nilai penuh

Format: `token = dark / light` (light = blok `[data-theme="light"]` bila berbeda; `-` = sama dengan dark).

### violet — Pop Violet

| token                          | dark                     | light                                 |
| ------------------------------ | ------------------------ | ------------------------------------- |
| bg                             | #0A0A11                  | #E9E7F2                               |
| win                            | #0E0D16                  | #EFEDF6                               |
| pane                           | #12121B                  | #FFFFFF                               |
| paneActive                     | #16151F                  | #FFFFFF                               |
| elev                           | #1E1C2A                  | #FFFFFF                               |
| elev2                          | #272536                  | #F1EFF8                               |
| text                           | #F6F5FB                  | #0F0E16                               |
| text2                          | #A6A2BA                  | #565269                               |
| text3                          | #6F6B84                  | **#8A8699** (v2 app: #6E6A7C — delta) |
| accent                         | #7B3FFF                  | #7B3FFF                               |
| accentInk                      | #A07BFF                  | #6629F0                               |
| green                          | #2ED67A                  | **#00B85C** (v2 app: #0E9F4E)         |
| amber                          | #FF9500                  | #FF9500                               |
| red                            | #FF3355                  | **#FF2D55** (v2 app: #E0203F)         |
| side                           | (var titlebar)           | #F6F2FF                               |
| pop / popInk / popOn           | #FF3DA5 / #FF6BBE / #fff | #FF3DA5 / **#E0137F** / #fff          |
| duoGrad                        | 135deg #7B3FFF→#FF3DA5   | sama                                  |
| accentGrad                     | 135deg #7B3FFF→#B43CFF   | sama                                  |
| greenInk/amberInk/redInk light | —                        | #0E9F4E / #C06A00 / #E0203F           |

### electric — Electric

dark: bg #090B0F, win #0D1016, pane #10131A, paneActive #141821, elev #1B202A, elev2 #242A36, text #F5F7FB, text2 #98A2B3, text3 #667085, accent #2563FF, accentInk #5B8CFF, green #1FC75A, amber #FF8A00, red #FF3B30; pop #A8E600/#C2F53A/#0D1015; duoGrad #2563FF→#A8E600; accentGrad #2563FF→#00A3FF.
light: side #F1F5FF, bg #E7EAF0, win #EDF0F5, pane/paneActive/elev #FFFFFF, elev2 #EFF2F7, text #0D1015, text2 #525B6A, text3 #858E9C, accentInk #1A4FE0, green **#00B34A** (v2 app: #0E8A3E), red #FF3B30; popInk **#4E8F00**; greenInk/amberInk/redInk = #0E8A3E / #B35A00 / #D92D20.

### aqua — Aqua Signal

dark: bg #080B0E, win #0C1115, pane #0F1418, paneActive #13191E, elev #1A2128, elev2 #232C34, text #F3F7FA, text2 #96A3AE, text3 #64717C, accent #0096E6, accentInk #3DB8FF, green #8BD42A, amber #FF9F0A, red #FF3B47; pop #FF6B57/#FF8A7A/#fff; duoGrad #0096E6→#FF6B57; accentGrad #0096E6→#00C2C7.
light: side #EEF8FB, bg #E5EBEF, win #ECF1F4, pane/paneActive/elev #FFFFFF, elev2 #EDF2F5, text #0B1115, text2 #4E5B66, text3 #83909A, accentInk #007FC4, green **#5CB800** (v2 app: #3F8F00), red #FF3347; popInk #E04A33; inks = #3F8F00 / #B86E00 / #D81F2E.

### green — Fresh Mint (DEFAULT v3; tanpa varian emerald/lime fase ini)

dark: bg #080D0B, win #0B120F, pane #0E1512, paneActive #121A16, elev #18221D, elev2 #212D27, text #F3F8F5, text2 #98A89F, text3 #66756C, accent **#00B86B**, accentInk #3BE39A, green #2ED67A, amber #FFA41B, red #FF3B5C; onAccent #fff; pop #FF4F8B/#FF7AA8/#fff; duoGrad #00A85F→#FF4F8B; accentGrad #00B86B→#00D6A4.
light: side #F8FAF8, bg #E6EEE9, win #EDF3EF, pane/paneActive/elev #FFFFFF, elev2 #EEF4F0, text #0C130F, text2 #4F5E55, text3 #85938A, accent **#00A85F**, accentInk #00804A, green #00B85C, amber #FF9500, red #FF2D55; popInk #E0226A; inks = #0E8A48 / #B86400 / #E0203F.

### yellow — Sunshine

dark: bg #0C0C0A, win #111110, pane #151513, paneActive #1A1A17, elev #22221E, elev2 #2B2B26, text #FAFAF5, text2 #A6A69A, text3 #727266, accent **#FFC400**, accentInk #FFD54A, green #2ED67A, amber #FF9500, red #FF3355; **onAccent #1F1700** (teks gelap di atas accent kuning); pop #7B3FFF/#A07BFF/#fff; duoGrad #FFC400→#7B3FFF; accentGrad #FFB300→#FFD84D.
light: side #FAFAF6, bg #EEEEE8, win #F4F4EF, pane/paneActive/elev #FFFFFF, elev2 #F3F3EC, text #13130F, text2 #56564C, text3 #8C8C80, accent #FFC400, accentInk **#8F6400**, green #00B85C, amber #FF9500, red #FF2D55; popInk #6629F0; inks = #0E9F4E / #C06A00 / #E0203F.

### orange — Tangerine

dark: bg #0D0B0B, win #131111, pane #171414, paneActive #1C1818, elev #242020, elev2 #2D2828, text #FBF7F5, text2 #ABA19D, text3 #766C68, accent **#FF6A00**, accentInk #FF9A4D, green #2ED67A, amber **#FFC400**, red #FF3355; onAccent #fff; pop #00BFB3/#3DE0D5/#062B29; duoGrad #FF6A00→#00BFB3; accentGrad #FF6A00→#FF3D6E.
light: side #FBF9F8, bg #F0EBE8, win #F6F2EF, pane/paneActive/elev #FFFFFF, elev2 #F6F0EC, text #16110E, text2 #5E534D, text3 #948781, accent #FF6A00, accentInk **#C94A00**, green #00B85C, amber **#FFB800**, red #FF2D55; popInk #00877E; inks = #0E9F4E / #A87400 / #E0203F.

## 4. Ikon file — colored vs monochrome (baris 1029–1036)

- Monokrom: `kindColor()` langsung mengembalikan `var(--text3)` — HANYA warna glyph file/folder yang menetral; UI/status lain tidak disentuh. Bentuk glyph tetap (`kindGlyph` per kategori, baris 1030).
- Colored: set per PALET (bukan per mode). Pemetaan kategori→warna: `code/doc/pdf/image` langsung, `video|audio→media`, `archive`, `data`, `config → #7D8AA8` (default/unknown juga #7D8AA8).

| palet    | code    | image   | media   | archive | data    | doc     | pdf     |
| -------- | ------- | ------- | ------- | ------- | ------- | ------- | ------- |
| violet   | #3B82F6 | #00D1C1 | #FF4FA3 | #FFC928 | #2ED67A | #FF6B3D | #FF3355 |
| electric | #2F80FF | #00C2A8 | #FF3D8B | #FFB400 | #1FC75A | #8B5CF6 | #FF3B30 |
| aqua     | #4F5BFF | #00C9A7 | #E040FB | #FFD60A | #8BD42A | #FF5C5C | #FF3B47 |
| green    | #3B82F6 | #00C9D1 | #FF4F8B | #FFC928 | #00C875 | #FF7A2F | #FF3355 |
| yellow   | #3D7BFF | #00C2A8 | #FF4FA3 | #FFB000 | #2ED67A | #8B5CF6 | #FF3B30 |
| orange   | #3B82F6 | #00BFB3 | #E040FB | #FFC928 | #2ED67A | #FF6A00 | #FF3355 |

Catatan: app.css existing punya `--ft*` per MODE (dark/light) hanya untuk palet violet-ish. Kontrak: `--ft*` menjadi per palet (nilai dark dari tabel di atas); set light existing dipertahankan sebagai override global light (deviasi terdokumentasi — mock v3 tidak membedakan mode untuk warna file; set light v2 dirancang untuk kontras di latar putih).

## 5. Ekstensi file — show extensions (baris 1670)

`const nm = up || ghost || s.sv.ext || isDir ? e.name : e.name.replace(/\.[^.]+$/, '') || e.name;`

- `ext=false` → strip extension TERAKHIR hanya pada tampilan (row `name`); nama/path asli tetap untuk operasi.
- Directory, up-entry (`..`), ghost, dan nama tanpa suffix tidak berubah (`|| e.name`).
- Multi-extension: hanya suffix terakhir terstrip (regex `/\.[^.]+$/`) — `.jsonl.gz` tampil `name.jsonl` — terdokumentasi sebagai perilaku desain.

## 6. Folders-first & sorting (baris 1041–1049)

```js
if (x.dir !== y.dir && this.state.sv.foldersFirst) return x.dir ? -1 : 1;  // SEBELUM arah sort
let r = … localeCompare numeric-aware / angka …;
if (!r) r = x.name.localeCompare(y.name);
return r * d;
```

- folders-first diterapkan TERLEPAS dari arah (folder tetap di atas saat descending).
- Tie-break: name localeCompare. Parent `..` = up-entry terpisah di depan (baris 1053), tidak ikut comparator.

## 7. Hidden files (baris 1043, 1615)

- Mock: filter dotfile frontend `(showHidden || name[0] !== '.')`, shortcut Ctrl+Shift+. — **mock saja**. APP-08 wajib policy provider nyata (dotfile Unix + atribut hidden/system Windows) sesuai kontrak produk; jangan ikuti pola dotfile-saja.

## 8. Density

- `--row`: compact 26px, comfortable 30px (baris 946), di-set inline ke documentElement.
- Acuan font default 13px (baris 47 screen `font-size:13px`). Skala 12/13/14 px datang dari spec task (tidak ada mekanisme font-scale di mock) → mekanisme aplikasi ditetapkan di kontrak (`--uiFont` + `data-ui-font`), row minimum mengikuti font.

## 9. Label & ID resmi (baris 1619–1625)

`Pop Violet=violet, Electric=electric, Aqua Signal=aqua, Fresh Mint=green, Sunshine=yellow, Tangerine=orange` — dipakai di command palette v3; sama dengan mapping kontrak produk. Density toggle: 'Comfortable Density' (compact↔comfortable).

## 10. MISSING / ambiguitas

- Set warna file light per palet: MISSING di mock (deviasi terdokumentasi §4).
- Mekanisme skala font 12/13/14: tidak ada di mock (ditetapkan kontrak §8).
- `data-green` default mock = 'Emerald' dari props (baris 945) — TIDAK dipakai; default green aplikasi = blok green standar (keputusan desain).
- Label chip settings modal mock tidak diekstrak per-kata (markup settings tidak dibaca penuh — hemat konteks); label resmi diambil dari command registry v3 §9 dan kontrak produk. Untuk APP-02 cukup; nilai token TIDAK ada yang berasal dari bagian ini.
