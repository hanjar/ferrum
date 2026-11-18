# Ferrum 0.2.7 — upgrade notes

For people moving from 0.2.6 to 0.2.7. Windows 10/11 x64 only. There is no auto-updater: you upgrade by running the new setup over the old install.
Coming from 0.2.5 or earlier? Read the [0.2.6 upgrade notes](../v0.2.6/UPGRADE-NOTES.md) and the ones before it first; everything in them still applies.

## Installing over 0.2.6

- Run `Ferrum_0.2.7_x64-setup.exe`. It installs **per machine** to `C:\Program Files\Ferrum`, asks for administrator rights once and replaces the 0.2.6 files. Nothing to uninstall first.
- **The installer is not signed.** Windows SmartScreen will warn. Check the SHA-256 shown on the release page (`Get-FileHash .\Ferrum_0.2.7_x64-setup.exe -Algorithm SHA256`), then choose More info > Run anyway.
- Finish or cancel running file operations and archive jobs first: the setup replaces `ferrum.exe`.
- The optional `FerrumIndexer` service is stopped before files are replaced and registered again afterwards. Its mode is kept.

## Your data

Settings, session, Directory Hotlist, Activity history, comments, sync profiles, view presets, AI provider setup and the `Rar.exe` path stay as they were; locations are in the [0.2.0 table](../v0.2.0/UPGRADE-NOTES.md#your-data). An upgrade never deletes data.

**No store or schema migration happens between 0.2.6 and 0.2.7.** New Recent settings (sources, watched folders, window) are added to `config.json` with defaults; existing values are kept. No change to `hotlist.json`, the Activity database (schema v13) or the automation store (schema 2).

## Behaviour that differs from 0.2.6

- **Ctrl+E** now opens the Recent view in the active tab (Backspace returns), and Ctrl+K with an empty input shows a "Recent files" group. The sources are in Settings > Recent files: Windows Recent and newly added items in watched folders (Downloads by default) are **on by default** and can be switched off there. A private session hides all of them.

## Not available in 0.2.7

Same as 0.2.6: Automation, "Browse..." folder button in Sync profiles, RAR creation without your own WinRAR, archive update in place, encrypted archive creation, multi-volume archives, search inside archives, MSI, code signing, auto-update, Linux and macOS builds.
