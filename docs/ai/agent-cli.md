# Ask AI agent (FER-490): agent CLI lokal dalam mode baca-saja

Pertanyaan tentang lampiran bisa dijawab oleh agent CLI lokal (Claude Code, Codex). Agent membaca
lampiran sendiri, jadi PDF, Office, dan file besar ikut terbaca. Kode: `src-tauri/src/ai/agent_cli/`,
UI: `src/lib/ai/agentCli.svelte.ts`, `ChatPanel`, `AttachmentBar`, `AgentCliSettings`.

## Alur satu pertanyaan

1. Webview mengirim `agent_ask` berisi id agent, pertanyaan, dan path lampiran. Webview tidak pernah
   memilih program, flag, atau folder.
2. Backend memeriksa agent:
   - agent didukung dan (khusus agent baca luas) sudah diaktifkan;
   - lokasi exe lolos cek ACL;
   - di profil uji: launch sama persis dengan `agent-cli-test.json`;
   - disclosure sudah disetujui.
3. Backend membuat folder kerja baru `<data>\agent-runs\<requestId>-<acak>\` lalu menyalin lampiran ke
   `files\`. Ini adalah **snapshot** (E1):
   - Sumber dibuka lewat `path_guard` FER-486 dan dibaca dari handle yang sudah dicek, sehingga tidak
     ada TOCTOU.
   - Symlink/junction tidak diikuti, dan placeholder cloud dilewati.
   - Handle setiap anak harus benar-benar berada di dalam folder yang sudah dicek (`final_path`).
   - Batas: 100 MB, 2000 entri (file dan subfolder dihitung bersama), kedalaman folder 8. Kalau
     terlampaui, pertanyaan ditolak.
   - Setiap file dicek sensitif (nama dan seluruh isi, per chunk). File sensitif di dalam folder
     tidak disalin dan disebut di bawah jawaban. File level atas yang sensitif wajib dicentang.
4. Agent dijalankan dengan cwd = folder kerja:
   - Claude tanpa `--add-dir` dan tetap `--restricted`.
   - Codex dengan `-C <folder kerja>`.
   - Prompt dikirim lewat STDIN dan menyebut path relatif `files\...`.
5. Jawaban di-stream sebagai teks. Folder kerja selalu dihapus, baik saat selesai, gagal, batal,
   maupun timeout.

## Status agent di mesin owner (Windows, 2026-10-09)

| Agent       | Versi   | Status                          | Alasan                                                                                       |
| ----------- | ------- | ------------------------------- | -------------------------------------------------------------------------------------------- |
| Claude Code | 2.1.295 | didukung                        | `--tools Read,Grep,Glob` + `--restricted`. Baca di luar folder kerja ditolak ("is outside"). |
| Codex       | 0.159.1 | **baca luas, nonaktif default** | Lihat bagian Codex. Hanya aktif lewat opt-in merah di Settings.                              |
| omp         | 18.4.12 | tidak didukung                  | Tidak ada sandbox atau mode izin baca-saja.                                                  |

Agent yang tidak didukung tidak pernah dijalankan, termasuk `--version`.

Exe yang file atau foldernya bisa ditulis oleh akun selain user saat ini, Administrators, SYSTEM,
atau TrustedInstaller akan ditolak (E3).

## Codex: kenapa "baca luas"

`--sandbox read-only` di Windows memakai sandbox _unelevated_ (restricted token). Sandbox ini
menolak tulis, tetapi tetap mengizinkan baca ke semua lokasi yang bisa dibaca user. Ferrum mencoba
membatasi baca dengan permission profile (`:minimal` + `:workspace_roots` = read). Uji di mesin owner
memakai `codex sandbox`, yang tidak memanggil model sehingga tanpa biaya:

- `windows.sandbox="unelevated"`: `Restricted read-only access requires the elevated Windows sandbox backend`
- `windows.sandbox="elevated"`: `elevated Windows sandbox requires effective ':root' read access`

Artinya, di Codex 0.159.1 pada Windows, baca **tidak bisa** dibatasi ke folder kerja. Karena itu Codex
tampil sebagai "baca luas — nonaktif" dan hanya bisa diaktifkan lewat centang di Settings → AI →
Agent CLI, dengan peringatan merah. Opt-in disimpan di backend (`agent-cli.json`, field `optedIn`).
Snapshot tetap dipakai untuk Codex.

## Uji manual owner: membuktikan pembatasan baca dengan agent nyata

Jalankan di PowerShell biasa (bukan profil uji). Gunakan file dummy saja.

```powershell
$t = Join-Path $env:TEMP "fer490-proof"; New-Item -ItemType Directory -Force "$t\work\files","$t\outside" | Out-Null
Set-Content "$t\work\files\inside.txt" "INSIDE-OK"
Set-Content "$t\outside\secret.txt" "OUTSIDE-SECRET"
```

**Claude** (sama dengan argumen Ferrum, tanpa `--add-dir`). Diharapkan: `INSIDE-OK` terbaca,
dan `secret.txt` ditolak dengan pesan "is outside".

```powershell
cd "$t\work"
"Baca files\inside.txt, lalu baca $t\outside\secret.txt. Laporkan isi atau galatnya." |
  & "$env:APPDATA\npm\node_modules\@anthropic-ai\claude-code\bin\claude.exe" -p --output-format text `
    --tools Read,Grep,Glob --allowedTools Read,Grep,Glob --permission-mode dontAsk --permission-prompts none `
    --restricted --strict-mcp-config --mcp-config '{\"mcpServers\":{}}' --setting-sources '' --safe-mode `
    --no-session-persistence --model haiku
```

**Codex**. Uji tanpa model dulu: sandbox baca terbatas harus menolak berjalan.

```powershell
$codex = "$env:APPDATA\npm\node_modules\@openai\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe"
& $codex sandbox -C "$t\work" -c 'permissions.ferrum.filesystem={":minimal"="read", ":workspace_roots"={"."="read"}}' `
  -c 'windows.sandbox="unelevated"' -P ferrum -- cmd.exe /c type "$t\outside\secret.txt"
```

- Kalau versi Codex yang baru **menjalankan** perintah dan `OUTSIDE-SECRET` tercetak, baca belum
  dibatasi.
- Kalau muncul "Access is denied" untuk `secret.txt` tetapi `type "$t\work\files\inside.txt"`
  berhasil, baca sudah dibatasi. Dalam kondisi itu Codex bisa dipertimbangkan naik status menjadi
  "didukung (baca terbatas)", tetapi harus lewat peninjauan keamanan dan memakai argumen permission
  profile di `args.rs`.

Hapus `$t` setelah selesai.

## Profil uji

Saat `FERRUM_PROFILE_DIR` aktif, agent asli tidak dideteksi dan tidak dijalankan.
`<profil>\config\agent-cli-test.json` menamai agent palsu (`node.exe` +
`src-tauri/src/ai/agent_cli/testdata/fake-agent.mjs`), dan file ini dibaca ulang setiap kali ada
pertanyaan. Smoke: `node scripts/smoke-ask-agent.mjs <ferrum.exe>`.

## Buka di terminal (FER-494): mode interaktif

Selain mode baca-saja di chat, Ferrum bisa **membuka** agent CLI secara interaktif di terminal yang
terlihat, di folder pilihan user. Setelah terminal terbuka, user yang memimpin. Ferrum hanya
mengantar. Kode: `src-tauri/src/ai/agent_cli/terminal.rs`, UI: `src/lib/ai/agentTerminal.ts`.

### Beda dua mode

|                | Baca-saja di chat (FER-490)                               | Buka di terminal (FER-494)                                                                      |
| -------------- | --------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| Siapa memimpin | Ferrum: satu pertanyaan, jawaban teks di chat             | User, di terminal, dengan sesi agent biasa                                                      |
| Agent          | Claude Code; Codex hanya lewat opt-in "baca luas"         | Semua agent yang ditemukan dan lolos cek ACL (claude, codex, omp, agy, opencode, gemini, aider) |
| Izin           | Dipaksa Ferrum: flag baca-saja, tanpa MCP/plugin/settings | Izin dan konfigurasi milik agent/user apa adanya. Ferrum tidak menambah flag apa pun            |
| Folder         | SALINAN lampiran di folder kerja sementara                | Folder asli (untuk file: folder induknya), hanya sebagai working directory                      |
| Pertanyaan     | STDIN agent                                               | Disalin ke clipboard; user menempelnya sendiri (Ctrl+V)                                         |
| Proses         | Tersembunyi, job object, timeout 5 menit, output dibatasi | Jendela terminal yang terlihat; tidak dibatasi Ferrum                                           |
| `--version`    | Hanya Claude/Codex, setelah lolos ACL                     | Tidak pernah dijalankan untuk agent lain; cukup path + cek ACL                                  |

### Titik masuk

- Command palette: satu command per agent yang tersedia, "Buka <Agent> di sini" (folder panel aktif),
  dengan keywords nama agent, "agent", "terminal ai".
- Menu konteks file/folder: "Buka di agent ▸" membuka daftar agent yang tersedia. Untuk file, yang dibuka
  folder induknya.
- Composer chat, saat tujuan Agent dipilih: tombol "Buka di terminal (<agent default terminal>)" memakai
  folder lampiran pertama. Kalau lampiran berasal dari beberapa folder, UI memberi tahu bahwa hanya
  folder pertama yang dibuka. Agent default terminal dipilih di Settings → AI → Agent CLI.

### Cara peluncuran

Contoh untuk folder `C:\proj;a %PATH% ^&b é` dan Claude Code di `C:\Users\u\.local\bin\claude.exe`:

1. `<wt.exe absolut> -d . C:\Users\u\.local\bin\claude.exe` (mis. `C:\Users\u\AppData\Local\Microsoft\WindowsApps\wt.exe`), dengan current directory proses = folder itu.
2. Kalau `wt` tidak ada: `C:\Users\u\.local\bin\claude.exe` langsung di console baru
   (`CREATE_NEW_CONSOLE`), current directory = folder. Preferensi terminal `cmd`/`powershell` juga
   memakai rute ini. Agent tidak pernah dijalankan lewat `cmd.exe` atau shell lain.

Untuk paket npm tanpa binary native, programnya `node.exe` dan path script menjadi argumen pertama.

Aturan keamanan:

- **Folder tidak pernah masuk command line.** `wt` memecah command line-nya pada `;`. Di mesin owner,
  `wt -d "C:\…\a;b x" cmd /c …` gagal, sedangkan `wt -d .` dengan cwd = folder yang sama berhasil
  (diuji dengan folder `a;b x %PATH% ^&c é 漢`). `Open Terminal Here` (F2-25) ikut diubah ke `-d .`
  karena alasan yang sama.
- **Tidak ada teks user di command line.** Request dari webview hanya berisi `agent`, `path`, `program`
  (`deny_unknown_fields`). Argumen interaktif tiap agent adalah konstanta di `AGENT_DEFS` (saat ini
  kosong untuk semua agent).
- Exe yang path-nya mengandung `;` atau diawali `-` tidak diberikan ke `wt`. Exe semacam itu hanya
  memakai rute console.
- Folder dicek dengan `path_guard::check_str` (menolak `\?\`, `\.\`, `\??\`, host UNC aneh, pipe,
  ADS). Selain itu folder tidak boleh mengandung karakter kontrol, `"`, `*?<>|`, segmen `.`/`..`, atau
  segmen yang berakhir titik/spasi. UNC biasa (`\server\share\…`) diizinkan. Karakter `;`, `%`, `^`,
  `&`, `!`, spasi, dan unicode diizinkan karena folder hanya menjadi working directory.
- Exe dicek ulang sebelum setiap peluncuran: `.exe` absolut yang ada, dan lolos cek ACL (E3).
- Flag pelonggar izin seperti `--dangerously-skip-permissions`, `bypassPermissions`, `--yolo`,
  `--approval-mode`, dan `--full-auto` tidak pernah dipakai. Tes memeriksa daftar `FORBIDDEN_FLAGS`.
- Variabel `FERRUM_*` dihapus dari environment proses terminal.

### Folder yang tidak tepercaya (review keamanan T1–T3)

Agent memuat **konfigurasi proyek** dari folder kerja atau leluhurnya. Isinya bisa menjalankan
perintah (hooks, server MCP, `auto-test`) atau mengalihkan API. Karena itu, sebelum membuka agent,
backend memeriksa folder dengan `symlink_metadata` saja (reparse point tidak diikuti):

- File konfigurasi agent itu (tabel `project_configs` di `AGENT_DEFS`), di folder dan leluhurnya.
  Pencarian berhenti di git root (folder yang berisi `.git`), maksimal 8 folder, atau di root volume.

  | Agent       | File yang dicari                                | Risiko                                                 |
  | ----------- | ----------------------------------------------- | ------------------------------------------------------ |
  | Claude Code | `.claude\settings*.json`, `.mcp.json`           | hooks dan server MCP menjalankan perintah              |
  | Codex       | `.codex\`                                       | konfigurasi proyek (MCP, perintah, alamat API)         |
  | opencode    | `opencode.json`, `opencode.jsonc`, `.opencode\` | MCP lokal dijalankan saat start, plugin, alamat API    |
  | Gemini CLI  | `.gemini\settings.json`                         | server MCP dijalankan saat start                       |
  | Aider       | `.aider.conf.yml`, `.env`                       | `yes-always`/`auto-test` + perintah, `OPENAI_API_BASE` |
  | omp, agy    | (belum ada daftar)                              | hanya pemeriksaan UNC/pemilik/tautan di bawah          |

- Folder di jaringan (UNC).
- Folder yang berupa tautan atau junction.
- Pemilik folder bukan user saat ini, Administrators, SYSTEM, atau TrustedInstaller (`acl::owner_trusted`).
  Ini juga mencakup T3: exe agent atau `node.exe` dijalankan dengan cwd = folder itu, dan pencarian
  DLL Windows ikut melihat cwd. Akibatnya, DLL yang ditanam di folder milik akun lain bisa ikut dimuat.
  Risikonya sama dengan menjalankan agent secara manual di folder itu.

Hasilnya dikirim ke UI sebagai `warnings`:

- **Aider, opencode, Gemini CLI, omp, agy**: belum ada yang dijalankan (`needsConfirm`). UI
  menampilkan dialog "Buka <agent> di folder ini?" berisi daftar temuan, dengan tombol **Batal**
  (fokus awal; Enter = Batal) dan **Buka tetap**. "Buka tetap" memanggil `agent_terminal_open` lagi
  dengan `confirmedUntrusted: true`.
- **Claude Code, Codex**: keduanya sudah punya dialog trust folder sendiri. Agent langsung dibuka dan
  temuan hanya ditampilkan sebagai catatan di toast.

**`wt.exe` (T2)** tidak lagi dicari dengan nama saja. Ferrum me-resolve path absolutnya: alias App
Execution `%LOCALAPPDATA%\Microsoft\WindowsApps\wt.exe`, lalu `wt.exe` di folder PATH yang absolut.
Path itu diperiksa dengan `acl::trusted_location` (aturan E3), lalu dijalankan dengan path absolut.
Kalau tidak ditemukan atau tidak lolos, Ferrum memakai jalur console. Aturan yang sama berlaku untuk
`Open Terminal Here` (`shell_open::resolve_wt`).

Perilaku alias di mesin owner:

- Alias adalah reparse point `IO_REPARSE_TAG_APPEXECLINK` (atribut `0x420`), bukan name surrogate.
  `is_symlink()` bernilai false, dan `metadata()` mengembalikan file 0 byte.
- `GetNamedSecurityInfoW` membaca security descriptor **alias itu sendiri**. Pemiliknya user, di
  folder WindowsApps milik user, sehingga `trusted_location` = OK.
- Paket aslinya ada di `C:\Program Files\WindowsApps`, yang dilindungi TrustedInstaller.
- Artinya, cek ACL ini menjamin bahwa alias tidak bisa ditukar akun lain. Cek ini tidak memeriksa
  isi paketnya.

### Agent dan lokasi yang dicari

| Agent       | Exe            | Lokasi umum (selain PATH)                                                                                                                                    | Catatan                                                             |
| ----------- | -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------- |
| Claude Code | `claude.exe`   | `%USERPROFILE%\.local\bin`, npm `@anthropic-ai\claude-code\bin\claude.exe` (atau `node.exe cli.js`)                                                          |                                                                     |
| Codex       | `codex.exe`    | npm vendor `codex-win32-x64\…\codex.exe` (atau `node.exe codex.js`)                                                                                          | Tanpa opt-in baca luas. Toast menyebut Codex bisa membaca file lain |
| omp         | `omp.exe`      | `%LOCALAPPDATA%\omp`                                                                                                                                         |                                                                     |
| opencode    | `opencode.exe` | npm `opencode-ai\node_modules\opencode-windows-x64\bin`, `%USERPROFILE%\scoop\shims`, `%LOCALAPPDATA%\Microsoft\WinGet\Links`, `%USERPROFILE%\.opencode\bin` | Di PC owner: tidak ditemukan                                        |
| Gemini CLI  | `gemini.exe`   | npm `@google\gemini-cli\dist\index.js` lewat `node.exe`                                                                                                      |                                                                     |
| Aider       | `aider.exe`    | PATH / `%USERPROFILE%\.local\bin` (pipx/uv)                                                                                                                  | Toast mengingatkan aider bisa mengubah dan meng-commit file         |

Shim `.cmd`/`.ps1` tidak pernah dijalankan.

### Profil uji

Saat `FERRUM_PROFILE_DIR` aktif, tidak ada yang diluncurkan. Agent harus sama dengan agent palsu di
`agent-cli-test.json`. Perintah yang _akan_ dijalankan dikembalikan sebagai `dryRun: true` dan ditambahkan
ke `<data>\agent-terminal-dry-run.jsonl`. E2E memakai fake launcher di mock IPC
(`e2e/agent-terminal.e2e.ts`).
