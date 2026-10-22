# Ferrum 0.2.2 — upgrade notes

For people moving from 0.2.1 to 0.2.2. Windows 10/11 x64 only. There is no auto-updater: you upgrade by running the new setup over the old install.
Coming from 0.2.0 or earlier? Read the [0.2.1 upgrade notes](../v0.2.1/UPGRADE-NOTES.md) first (they include the automation store migration); everything in them still applies.

## Installing over 0.2.1

- Run `Ferrum_0.2.2_x64-setup.exe`. It installs **per machine** to `C:\Program Files\Ferrum`, asks for administrator rights once and replaces the 0.2.1 files. Same installer family, so there is nothing to uninstall first.
- **The installer is not signed.** Windows SmartScreen will warn. Check the SHA-256 shown on the release page (`Get-FileHash .\Ferrum_0.2.2_x64-setup.exe -Algorithm SHA256`), then choose More info > Run anyway.
- Finish or cancel running file operations and archive jobs first: the setup replaces `ferrum.exe`.
- The optional `FerrumIndexer` service is stopped before files are replaced and registered again afterwards. An existing indexer configuration (mode) is kept.

## Your data

Your settings, session, Directory Hotlist, Activity history, per-folder comments, sync profiles, AI provider setup and the `Rar.exe` path stay as they were; the locations are in the [0.2.0 table](../v0.2.0/UPGRADE-NOTES.md#your-data). An upgrade never deletes data.

**No store or schema migration happens between 0.2.1 and 0.2.2.** The diff since 0.2.1 has no change to `config.json`, `hotlist.json`, the Activity database (`activity.sqlite3`, schema v13) or the automation store (`automation.db`, schema 2). The only new stored value is a short list of your last recent command ids (ids only, no paths), kept by the app window under the key `ferrum.recentCommands.v1`; if it is missing or unreadable it is simply empty. Because nothing was migrated, going back to 0.2.1 would not hit a newer store, although downgrading is still not supported.

## Behaviour that differs from 0.2.1

- **Enter on a focused button presses that button.** In Delete, Pack and similar dialogs the main action is no longer started by Enter while Cancel (or a checkbox) has focus. In Copy / Move, choosing Overwrite needs a second Enter.
- **Alt+letter** is now used for dialog buttons and menu-bar menus (Alt+F, Alt+M, Alt+C, Alt+V, Alt+S, Alt+W). Alt+D and Alt+B keep their old meaning. Ctrl+C / Ctrl+X / Ctrl+V in a pane now act on **files** (Windows clipboard); Ctrl+1/2/3 still copy names, paths and details.
- **Delete runs as a background job** with a Cancel button; progress dialogs can be sent to the background with the Background button or Esc. Alt+B shows the jobs.
- **Closing Ferrum** (X, Alt+F4) asks while jobs are running.
- **Extract** no longer overwrites a file that appears at the destination while it runs; it skips or renames it unless you chose Overwrite.
- **Enter on a file inside an archive** opens it (extracted to a temporary folder).
- **AI panel:** new actions (7z, go to a path, delete to the Recycle Bin, chains). AI delete never deletes permanently.

## Not available in 0.2.2

Automation (schedules and folder watching: the code is in the build but switched off and there is no screen to create one), "Browse..." folder button in Sync profiles, RAR creation without your own WinRAR (permanent), archive update in place, encrypted archive creation, multi-volume archives, search inside archives, MSI, code signing, auto-update, Linux and macOS builds.
Open problems are in [KNOWN-ISSUES.md](KNOWN-ISSUES.md).
