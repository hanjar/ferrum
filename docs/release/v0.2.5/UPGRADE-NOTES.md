# Ferrum 0.2.5 — upgrade notes

For people moving from 0.2.4 to 0.2.5. Windows 10/11 x64 only. There is no auto-updater: you upgrade by running the new setup over the old install.
Coming from 0.2.3 or earlier? Read the [0.2.4 upgrade notes](../v0.2.4/UPGRADE-NOTES.md) and the ones before it first; everything in them still applies.

## Installing over 0.2.4

- Run `Ferrum_0.2.5_x64-setup.exe`. It installs **per machine** to `C:\Program Files\Ferrum`, asks for administrator rights once and replaces the 0.2.4 files. Nothing to uninstall first.
- **The installer is not signed.** Windows SmartScreen will warn. Check the SHA-256 shown on the release page (`Get-FileHash .\Ferrum_0.2.5_x64-setup.exe -Algorithm SHA256`), then choose More info > Run anyway.
- Finish or cancel running file operations and archive jobs first: the setup replaces `ferrum.exe`.
- The optional `FerrumIndexer` service is stopped before files are replaced and registered again afterwards. Its mode is kept.

## Your data

Settings, session, Directory Hotlist, Activity history, comments, sync profiles, AI provider setup and the `Rar.exe` path stay as they were; locations are in the [0.2.0 table](../v0.2.0/UPGRADE-NOTES.md#your-data). An upgrade never deletes data.

**No store or schema migration happens between 0.2.4 and 0.2.5.** No change to `config.json`, `hotlist.json`, the Activity database (schema v13) or the automation store (schema 2). View presets gain an optional thumbnail size; presets saved by 0.2.4 load with Medium.

## Behaviour that differs from 0.2.4

- **F3 on an image** opens the new image viewer instead of Quick View. F3 on other files, and on images inside an archive, still opens Quick View.
- **F5 out of a folder inside an archive** copies the selected items straight into the target. In 0.2.4 the archive's folder path was recreated under the target.
- **Ctrl+wheel over Thumbnail View** changes the thumbnail size.

## Not available in 0.2.5

Same as 0.2.4: Automation, "Browse..." folder button in Sync profiles, RAR creation without your own WinRAR, archive update in place, encrypted archive creation, multi-volume archives, search inside archives, MSI, code signing, auto-update, Linux and macOS builds.
