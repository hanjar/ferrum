# Ferrum 0.2.1 — upgrade notes

For people moving from 0.2.0 to 0.2.1. Windows 10/11 x64 only. There is no auto-updater: you upgrade by running the new setup over the old install.
Coming from 0.1.0 or RC1? Read the [0.2.0 upgrade notes](../v0.2.0/UPGRADE-NOTES.md) first; everything in them still applies.

## Installing over 0.2.0

- Run `Ferrum_0.2.1_x64-setup.exe`. It installs **per machine** to `C:\Program Files\Ferrum`, asks for administrator rights once and replaces the 0.2.0 files. Same installer family, so there is nothing to uninstall first.
- **The installer is not signed.** Windows SmartScreen will warn. Check the SHA-256 shown on the release page (`Get-FileHash .\Ferrum_0.2.1_x64-setup.exe -Algorithm SHA256`), then choose More info > Run anyway.
- Finish or cancel running file operations and archive jobs first: the setup replaces `ferrum.exe`.
- The optional `FerrumIndexer` service is stopped before files are replaced and registered again afterwards. An existing indexer configuration (mode) is kept and the mode question is not asked again.
- Downgrading is not supported (see "Automation store" below).

## Your data

Your settings, session, Directory Hotlist, Activity history, per-folder comments, AI provider setup and the `Rar.exe` path stay as they were; the locations are in the [0.2.0 table](../v0.2.0/UPGRADE-NOTES.md#your-data). An upgrade never deletes data. There is no schema or format change to `config.json`, `hotlist.json` or the Activity database (`activity.sqlite3`, schema v13) in 0.2.1.

### Automation store (sync profiles): schema 1 to 2

- Sync profiles, their revisions and sync baselines live in a separate SQLite store named `automation.db`, in the `automation` folder of Ferrum's local application data. Ferrum 0.2.0 already created it (schema version 1).
- The first start of 0.2.1 migrates it to **schema version 2** (new tables for automations, approvals, queue, runs and events). Your existing sync profiles are kept. Before changing anything, Ferrum writes a consistent backup next to it, `automation.db.bak-v1`.
- The new tables are empty and unused in this release, because the Automation screen is not available (see below). A small additive change (the `baseline_gen` table and a `merged_from` column on the queue) is applied on open when missing.
- **Going back to 0.2.0 after this migration:** the older build opens the newer store read-only and never writes to it, so sync profile saves and runs would not work there. Do not downgrade; if you must, restore `automation.db.bak-v1` over `automation.db` (and delete `automation.db-wal` and `automation.db-shm`) with Ferrum closed. Changes made to sync profiles after the upgrade are lost by doing that.

## Behaviour that differs from 0.2.0

- **New Sync profiles page** under "Sync & Backup" in the sidebar. Profiles run only when you press Run now. See [RELEASE-NOTES.md](RELEASE-NOTES.md).
- **Sync refuses protected folders.** A sync profile whose folder is, or lies inside, Ferrum's config, data or Activity folders or a Windows Startup folder (per user or all users), or that sits on a share pointing at this computer, shows "not ready" and its runs fail until you edit the folder. A written folder also may not contain such a folder (for example a profile that writes to your whole user profile).
- **The Startup folders are never written** by copy, move, extract, archive create, split, combine, encode, decode, checksum or selection save. Moving or renaming a folder so that it ends up in, or above, Startup is refused. Reading, opening and deleting still work.
- **Reserved device names** (`nul`, `aux.c`, ...): such existing files now copy and move with their content, but cannot be sent to the Recycle Bin; delete them permanently.
- **`Rar.exe` behind a junction or symbolic link** in any parent folder is refused. If RAR creation disappears after upgrading, open Settings > Archive and pick the real folder (for example `C:\Program Files\WinRAR`, not a Scoop `current` junction).
- **Sync overwrites** re-check the target at the moment of the swap and leave a file that changed meanwhile alone (reported as changed); a file held open for writing is reported as a sharing violation.

## Not available in 0.2.1

Automation (schedules and folder watching: the code is in the build but switched off and there is no screen to create one), "Browse..." folder button in Sync profiles, RAR creation without your own WinRAR (permanent), archive update in place, encrypted archive creation, multi-volume archives, search inside archives, MSI, code signing, auto-update, Linux and macOS builds.
Open problems are in [KNOWN-ISSUES.md](KNOWN-ISSUES.md).
