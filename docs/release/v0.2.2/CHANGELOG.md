# Ferrum 0.2.2 — changelog

Changes since 0.2.1 (`docs/release/v0.2.1/`). Windows 10/11 x64 only. Entries refer to public pull requests (`#n` on `hanjar/ferrum`).
Everything in the 0.2.1 [CHANGELOG](../v0.2.1/CHANGELOG.md) and 0.2.0 [CHANGELOG](../v0.2.0/CHANGELOG.md) is still part of the product. This file lists only what is new or changed.

## Features

- **Copy, cut and paste files with Windows Explorer** (#47): Ctrl+C / Ctrl+X on the selection of the active pane puts the files on the Windows clipboard (`CF_HDROP` plus `Preferred DropEffect`), so Explorer can paste them. Ctrl+V pastes files copied in Explorer (or in Ferrum) into the active folder, through the same plan, conflict dialog, queue, progress and guards as F5 / F6. After a cut-and-paste move finishes, the clipboard is emptied if it still holds the same files. Copy Names / Paths / Details (Ctrl+1/2/3) are unchanged. The commands are in the Mark menu and in the command palette.
- **Open a file inside an archive** (#50): Enter on a member of an archive extracts it to a temporary folder and opens it in its Windows application. Edits are never written back into the archive and reusing a temporary copy never deletes your edits. Extracted files carry the Mark of the Web; if that cannot be set the open is refused.
- **AI: create 7z and choose a compression level** (#49): the AI panel can create 7z archives (not only ZIP) and honours a compression level (store, fast, normal, maximum, ultra).
- **AI: go to a path and delete to the Recycle Bin, and chains** (#52): the AI panel can navigate a pane to a typed path and delete the selection **to the Recycle Bin only** (never permanently), each with a confirmation card. A request may be a short chain of up to four steps in a fixed order (go to a path, create an archive, delete the original). The delete step runs only after the archive was created and tested. AI delete refuses when it cannot promise the Recycle Bin (bin disabled or size 0, "Don't move files to the Recycle Bin", item too big for the bin quota).
- **AI: clearer provider errors, and a hint for size questions** (#56): when the provider rejects a request, the panel shows the provider's own message (with control characters and keys removed) instead of a generic error. Questions about folder sizes point to Disk Usage.
- **Keyboard-first dialogs and menus** (#51, #53, #60): every dialog now traps Tab, closes with Esc and returns focus to where it came from. Buttons and fields have underlined **Alt access letters** (for example Alt+P permanent delete, Alt+Z / Alt+7 / Alt+R in Pack). The menu bar opens with Alt+F, Alt+M, Alt+C, Alt+V, Alt+S, Alt+W. Context menus have Home / End and access letters; the Menu key and Shift+F10 open the menu on the cursor row. Pane tabs are a keyboard tab list: Ctrl+Tab and Ctrl+Shift+Tab cycle them. Settings, Hotlist, Search, Compare, Sync profiles, Automation page, Disk Usage and the AI panel follow the same rules. Pressing Enter on a focused button now activates that button, not the dialog's main action (Enter on Cancel in Delete no longer deletes).
- **Recent commands in Ctrl+K** (#53): the empty command palette lists the last three commands you ran. Only command ids are stored, never paths.
- **Background jobs dock** (#55): the operations panel (Alt+B) now has a dock of background jobs, with a badge on the title bar. Checksum verify and the hash of an archive member can run in the background; progress dialogs have a **Background** button and Esc sends them to the background. Closing Ferrum counts running jobs.
- **Large deletes run as background jobs** (#61): the Delete dialog no longer freezes on a big delete. It runs as a job with progress and a Cancel button and keeps focus in the dialog while it runs. Permanent delete walks the tree without following links and stops between top-level items when cancelled.

## Fixes and safety

- **Disk Usage follows the active pane** (#48): opening Disk Usage now starts from the folder of the active pane instead of a stale earlier result.
- **Extract never replaces files silently** (#59): extracted files are now published with a no-replace rename. A file that appears between the existence check and the commit is skipped, renamed (`name (2)`) or, only if you chose Overwrite, replaced through a checked swap with a backup. Two overlapping extracts no longer lose files.
- **Close guard** (#58): the window X button and Alt+F4 now ask for confirmation while jobs run, and every way of closing the main window goes through the same question. There is an escape hatch if the page does not answer.
- **Enter no longer runs a dialog's primary action from a button** (#51, #60): see Keyboard above. Overwrite in Copy / Move needs a second Enter.

## CI and tooling

- Playwright end-to-end tests run against a production build (`vite preview`) instead of the dev server (#57), and keep a trace and screenshot when a test fails (#54).

## Internal, not enabled (Automation)

Automation code stays in the build as in 0.2.1. **None of it is available to users in 0.2.2.** The Automation UI is behind the build flag `VITE_FERRUM_AUTOMATION_UI`, which is off in release builds (the sidebar entry is greyed out).

## Not included in this release

- Automation (see above), MSI installer, code signing, auto-update, Linux and macOS.
