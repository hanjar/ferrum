# Ferrum 0.2.7 — changelog

Changes since 0.2.6 (`docs/release/v0.2.6/`). Windows 10/11 x64 only. Entries refer to public pull requests (`#n` on `hanjar/ferrum`).
Everything in the 0.2.6 [CHANGELOG](../v0.2.6/CHANGELOG.md) and earlier is still part of the product. This file lists only what is new or changed.

## New

- **Recent view (Ctrl+E)** (#80): Ctrl+E, or Recent in the Commands menu, turns the active tab into a read-only list of recently used files and folders, newest first. Enter opens the item, Ctrl+Enter reveals it in its folder, Backspace returns to the folder you came from.
- **Recent files in Ctrl+K** (#80): with an empty input, the command palette shows a "Recent files" group.
- **Sources** (#80): Ferrum's own Activity history, Windows Recent, and newly added items in watched folders (default: Downloads, top level only, by creation time within 1, 3, 7, 14 or 30 days). Windows Recent and the watched folders each have a switch in Settings > Recent files. Shortcuts in Windows Recent are read without resolving them, so there is no disk search and no network access to find a target. A private session hides every source.

## Internal, not enabled (Automation)

As in 0.2.5, the Automation UI is behind the build flag `VITE_FERRUM_AUTOMATION_UI`, which is off in release builds.

## Not included in this release

- Automation (see above), MSI installer, code signing, auto-update, Linux and macOS.
