# Test profile (`FERRUM_PROFILE_DIR`)

Tauri resolves `app_config_dir`, `app_data_dir` and `app_local_data_dir` through the Windows Known-Folder API, which
ignores `APPDATA` / `LOCALAPPDATA` overrides. Smoke and measurement runs therefore used to write tab sessions (and
Recent) into the real `%APPDATA%\dev.ferrum.app` profile (FER-457).

When the environment variable `FERRUM_PROFILE_DIR` is set to a non-empty path, the backend roots EVERYTHING it persists
there (`src-tauri/src/profile.rs`):

| Path             | Contents                                               |
| ---------------- | ------------------------------------------------------ |
| `<dir>/config`   | `config.json` (prefs + tab session), AI provider setup |
| `<dir>/data`     | AI execution journal, `sync-baselines/`                |
| `<dir>/activity` | Activity (Recent/Frequent) SQLite database             |
| `<dir>/webview`  | WebView2 user-data folder (localStorage, caches)       |

Rules:

- For testing and measurement only. Unset or blank means the normal OS locations; nothing else reads it.
- `FERRUM_ACTIVITY_DIR` (older, activity-only) still wins for the Activity store when both are set.
- Scripts that spawn `ferrum.exe` use `scripts/lib/test-profile.mjs`: `profileEnv(tempDir)` for the child env and
  `snapshotRealProfile()` / `verifyRealProfile()` to hash the real config/data/activity files before and after
  (`smoke-activity`, `smoke-archive` fail the run if they changed; `measure-app`, `profile-mem` exit with code 4).
- Scripts that attach to a running app (`ai-desktop-smoke*.mjs`) expect it to be launched with
  `$env:FERRUM_PROFILE_DIR = "$env:TEMP/ferrum-smoke-profile"`.
- Hash only while no real Ferrum is running; a running app legitimately rewrites its own session.

## Safety rules (FER-457 review)

- **Fail-closed validation.** `FERRUM_PROFILE_DIR` and `FERRUM_ACTIVITY_DIR` must be an absolute path on a LOCAL drive
  (`X:\...`). UNC, `\?\` / `\.\` device paths, WebDAV (`DavWWWRoot`, `@SSL`), `..` components, relative paths and
  network/unknown drives are rejected: the app logs one line naming the variable (never its value) and exits with code 2. It never falls back to the real profile.
- **Visible marker.** With the profile active the window title ends with "- Profil uji" and the log has one line saying
  so.
- **Real AI key untouched.** The API key lives in the shared Windows Credential Manager, so Settings > AI disables the
  key field and "Clear key" (reason shown) and the backend refuses `ai_credential_save` / `ai_credential_clear`.
  Reading the existing key (for Test connection) still works.
- **Extra proof.** `snapshotRealProfile()` also compares file list + size + mtime of the real WebView2 web storage
  (`%LOCALAPPDATA%\dev.ferrum.app\EBWebView\Default\{Local Storage,IndexedDB,Session Storage}`).
