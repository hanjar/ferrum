# Ferrum 0.2.9 — changelog

Changes since 0.2.8 (`docs/release/v0.2.8/`). Windows 10/11 x64 only.
Everything in the 0.2.8 [CHANGELOG](../v0.2.8/CHANGELOG.md) and earlier is still part of the product. This file lists only what is new or changed.

## Changed

- **Delete in the Recent view** (FER-600): F8 and Shift+F8 now work in the Recent view (Ctrl+E). They show the normal confirm dialog and use the Recycle Bin (permanent delete only when you choose it). They act on the items' own full paths (marked items, else the cursor item) and remove the deleted items from the list. Other changes (copy, move, rename, new folder, paste, pack) stay disabled in Recent.
- **Ctrl+Right / Ctrl+Left in the Recent view** (FER-600): a folder opens itself in the other pane; a file opens its parent folder there with the cursor on the file.

## Internal, not enabled (Automation)

As in 0.2.5, the Automation UI is behind the build flag `VITE_FERRUM_AUTOMATION_UI`, which is off in release builds.

## Not included in this release

- Automation (see above), MSI installer, code signing, auto-update, Linux and macOS.
