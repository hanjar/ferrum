# Ferrum 0.2.4 — changelog

Changes since 0.2.3 (`docs/release/v0.2.3/`). Windows 10/11 x64 only. Entries refer to public pull requests (`#n` on `hanjar/ferrum`).
Everything in the 0.2.3 [CHANGELOG](../v0.2.3/CHANGELOG.md) and earlier is still part of the product. This file lists only what is new or changed.

## Features

- **Drag files out of Ferrum** (#71): drag one or many files from a pane into a browser upload box (for example an e-mail attachment), Explorer, the desktop or any other app that accepts files. Ferrum starts a native Windows shell drag (`SHCreateDataObject` + `SHDoDragDrop`) that offers **copy only**, so the target can never move or delete the originals. The dragged paths pass the same checks as Ctrl+C; up to 10,000 items from one folder. New module `src-tauri/src/drag_out.rs`.
- **Drop files from other apps onto a pane** (#71): files dragged from Explorer or another app are copied into the pane under the cursor, with the usual conflict dialog. Dragging between Ferrum's two panes still copies, and Shift still moves; dropping on the pane the drag started from does nothing.
- **Shift+F4 creates a new file** (#69), as in Total Commander: a **New file** dialog asks for a name, creates an empty file in the active pane, puts the cursor on it and opens it with its default app (executables still ask for confirmation first). Also in the file context menu and the command palette (Ctrl+K).

## Fixes and safety

- **New file never overwrites** (#69): the file is created with "create new" semantics, so a name that appears in the meantime, or a dangling link with that name, is never followed or truncated. An existing folder with the same name is reported as "already exists".

## Internal, not enabled (Automation)

As in 0.2.3, the Automation UI is behind the build flag `VITE_FERRUM_AUTOMATION_UI`, which is off in release builds.

## Not included in this release

- Automation (see above), MSI installer, code signing, auto-update, Linux and macOS.
