# Ferrum 0.2.3 — upgrade notes

For people moving from 0.2.2 to 0.2.3. Windows 10/11 x64 only. There is no auto-updater: you upgrade by running the new setup over the old install.
Coming from 0.2.1 or earlier? Read the [0.2.2 upgrade notes](../v0.2.2/UPGRADE-NOTES.md) and the [0.2.1 upgrade notes](../v0.2.1/UPGRADE-NOTES.md) first; everything in them still applies.

## Installing over 0.2.2

- Run `Ferrum_0.2.3_x64-setup.exe`. It installs **per machine** to `C:\Program Files\Ferrum`, asks for administrator rights once and replaces the 0.2.2 files. Nothing to uninstall first.
- **The installer is not signed.** Windows SmartScreen will warn. Check the SHA-256 shown on the release page (`Get-FileHash .\Ferrum_0.2.3_x64-setup.exe -Algorithm SHA256`), then choose More info > Run anyway.
- Finish or cancel running file operations and archive jobs first: the setup replaces `ferrum.exe`.
- The optional `FerrumIndexer` service is stopped before files are replaced and registered again afterwards. Its mode is kept.

## Your data

Settings, session, Directory Hotlist, Activity history, comments, sync profiles, AI provider setup and the `Rar.exe` path stay as they were; locations are in the [0.2.0 table](../v0.2.0/UPGRADE-NOTES.md#your-data). An upgrade never deletes data.

**No store or schema migration happens between 0.2.2 and 0.2.3.** No change to `config.json`, `hotlist.json`, the Activity database (schema v13) or the automation store (schema 2).

## Behaviour that differs from 0.2.2

- **Pack dialog:** new 7z level **Ultra (text)** (Alt+U). Creating such an archive needs up to about 192 MiB of memory per file being compressed; extracting it needs the same.
- **7z archives that need very much memory are refused.** Extract and Test refuse a 7z whose coders need more than 1 GiB per block, and an encoded header that needs more than 64 MiB. Such archives are almost always hostile or broken; 7-Zip itself can still open a legitimate one.
- **Permanent delete** can now stop in the middle of a large folder when you press Cancel. What was not deleted yet stays in place.

## Not available in 0.2.3

Same as 0.2.2: Automation, "Browse..." folder button in Sync profiles, RAR creation without your own WinRAR, archive update in place, encrypted archive creation, multi-volume archives, search inside archives, MSI, code signing, auto-update, Linux and macOS builds.
