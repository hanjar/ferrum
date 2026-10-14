# Third-Party Notices and Licenses

Ferrum incorporates third-party software components, fonts, and libraries. The complete, machine-generated notice, with the name, version, SPDX license, source URL, and full license text of every Rust crate and npm package that ships with Ferrum, is in **[THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt)**. The installer places that file, `LICENSE-MIT`, `LICENSE-APACHE`, and the `licenses/` folder next to the application, and Settings has a button that opens it.

This page is a short, hand-written summary. `THIRD-PARTY-NOTICES.txt` is the authoritative document.

## Highlights

- **UnRAR** (`unrar` / `unrar_sys`): Alexander Roshal's freeware UnRAR source is used to read RAR archives. Its license (full text in [licenses/UnRAR-license.txt](licenses/UnRAR-license.txt)) allows use in any software that only handles RAR archives; it does not permit developing a RAR-compatible archiver.
- **WinRAR / Rar.exe**: Ferrum does not bundle or distribute WinRAR or any `Rar.exe`. RAR creation runs a user-provided, licensed WinRAR installation configured in Settings. WinRAR licensing remains the user's responsibility.
- **Geist and Geist Mono** fonts: SIL Open Font License 1.1.
- **PDF.js** (`pdfjs-dist`): Apache-2.0.
- **MPL-2.0** components (for example `cssparser`, `selectors`) are used unmodified; their source is available at the URLs listed in the notice.
- **SQLite** (bundled by `rusqlite`): public domain.
- Everything else is permissively licensed (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, and similar). The generator refuses GPL, AGPL, LGPL, SSPL, BUSL, and unknown licenses.

## Regenerating the notice

```powershell
npm ci
node scripts/generate-third-party-notices.mjs          # rewrite THIRD-PARTY-NOTICES.txt
node scripts/generate-third-party-notices.mjs --check  # fail if it is out of date (CI does this)
```

Run it whenever `src-tauri/Cargo.lock` or `package-lock.json` changes. See `scripts/generate-third-party-notices.mjs` for the sources and rules.
