# Ferrum 0.2.9 — upgrade notes

For people moving from 0.2.8 to 0.2.9. Windows 10/11 x64 only. There is no auto-updater: you upgrade by running the new setup over the old install.
Coming from 0.2.7 or earlier? Read the [0.2.8 upgrade notes](../v0.2.8/UPGRADE-NOTES.md) and the ones before it first; everything in them still applies.

## Installing over 0.2.8

- Run `Ferrum_0.2.9_x64-setup.exe`. It installs **per machine** to `C:\Program Files\Ferrum`, asks for administrator rights once and replaces the 0.2.8 files. Nothing to uninstall first.
- **The installer is not signed.** Windows SmartScreen will warn. Check the SHA-256 shown on the release page (`Get-FileHash .\Ferrum_0.2.9_x64-setup.exe -Algorithm SHA256`), then choose More info > Run anyway.
- Finish or cancel running file operations and archive jobs first: the setup replaces `ferrum.exe`.
- The optional `FerrumIndexer` service is stopped before files are replaced and registered again afterwards. Its mode is kept.

## Your data

Settings, session, Directory Hotlist, Activity history, comments, sync profiles, view presets, AI provider setup and the `Rar.exe` path stay as they were; locations are in the [0.2.0 table](../v0.2.0/UPGRADE-NOTES.md#your-data). An upgrade never deletes data.

**No store or schema migration happens between 0.2.8 and 0.2.9.** No change to `config.json`, `hotlist.json`, the Activity database (schema v13) or the automation store (schema 2).

## Behaviour that differs from 0.2.8

- **Recent view (Ctrl+E)**: F8 and Shift+F8 now delete the marked items (or the cursor item) with the normal confirm dialog and the Recycle Bin, and the items leave the list. Ctrl+Right / Ctrl+Left open a folder itself, or a file's parent folder with the cursor on the file, in the other pane. Other changes (copy, move, rename, new folder, paste, pack) remain disabled in Recent.

## Not available in 0.2.9

Same as 0.2.8: Automation, "Browse..." folder button in Sync profiles, RAR creation without your own WinRAR, archive update in place, encrypted archive creation, multi-volume archives, search inside archives, MSI, code signing, auto-update, Linux and macOS builds.
