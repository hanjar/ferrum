# Ferrum 0.2.6 — changelog

Changes since 0.2.5 (`docs/release/v0.2.5/`). Windows 10/11 x64 only. Entries refer to public pull requests (`#n` on `hanjar/ferrum`).
Everything in the 0.2.5 [CHANGELOG](../v0.2.5/CHANGELOG.md) and earlier is still part of the product. This file lists only what is new or changed.

## Fixes

- **F7 puts the cursor on the new folder** (#77): after New folder (F7) the cursor now moves to the folder that was just created, so Enter opens it straight away. Before, the cursor stayed on the row it was on. No items are marked. This works the same way as Shift+F4 for a new file.

## Internal, not enabled (Automation)

As in 0.2.5, the Automation UI is behind the build flag `VITE_FERRUM_AUTOMATION_UI`, which is off in release builds.

## Not included in this release

- Automation (see above), MSI installer, code signing, auto-update, Linux and macOS.
