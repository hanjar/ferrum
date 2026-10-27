# Ferrum 0.2.3 — changelog

Changes since 0.2.2 (`docs/release/v0.2.2/`). Windows 10/11 x64 only. Entries refer to public pull requests (`#n` on `hanjar/ferrum`).
Everything in the 0.2.2 [CHANGELOG](../v0.2.2/CHANGELOG.md) and earlier is still part of the product. This file lists only what is new or changed.

## Features

- **7z "Ultra (text)" compression level** (#63): the Pack dialog has a new 7z level, **Ultra (text)** (Alt+U), that uses PPMd (order 32, like 7-Zip `-mx=9 -m0=PPMd`). It usually gives smaller archives than LZMA2 for text, logs, source code and CSV. The PPMd model memory is sized per file, as 7-Zip does, and capped at 192 MiB. The AI panel can choose it too (`ultra-text`). ZIP and RAR refuse this level. New dependency: `ppmd-rust` 1.5.2 (CC0-1.0 OR MIT-0).

## Fixes and safety

- **Hostile 7z archives can no longer exhaust memory** (#65): before extracting or testing a 7z, the memory that its LZMA, LZMA2 and PPMd coders would need is summed per block and refused above 1 GiB. An encoded 7z header is checked before it is decoded (coder memory up to 64 MiB, header sizes up to 128 MiB, bounded counts). AES-encrypted 7z with an unreasonable key-derivation round count is refused instead of hanging.
- **Hostile 7z headers can no longer crash Ferrum** (#67): a tiny 7z that declares billions of files, streams or coders used to make the 7z library abort the whole process. Ferrum now ships a patched copy of `sevenz-rust2` 0.20.2 that checks every header count before allocating, rejects invalid coder graphs (cycles, unbound streams) and turns the remaining panics into errors. The patch is listed in `src-tauri/vendor/sevenz-rust2/FERRUM-PATCHES.md`; the main fix was also accepted upstream.
- **Permanent delete is handle-relative and can be cancelled per item** (#66): permanent delete now opens every entry relative to its parent folder without following links, checks that it is still the same file (file id) before deleting, and uses POSIX delete semantics. A folder swapped for a link during the delete is not followed. On network or unknown file systems the identity check is mandatory. Cancel now takes effect between entries, not only between top-level items, and progress reports bytes.

## Internal, not enabled (Automation)

As in 0.2.2, the Automation UI is behind the build flag `VITE_FERRUM_AUTOMATION_UI`, which is off in release builds.

## Not included in this release

- Automation (see above), MSI installer, code signing, auto-update, Linux and macOS.
