# Activity identity reconciliation (ACT-06)

Store schema v11, crate `ferrum-activity-store`. The store never touches the filesystem: callers pass receipts of changes Ferrum performed and results of stats they ran themselves (lazy validation). Entity id is distinct from locator; identity is never inferred from path alone when stronger evidence exists, and is never merged when evidence is insufficient.

## Model

- Entity key: `(volume_id, locator_key)`. `volume_id` should be a stable volume identity (volume serial/GUID), not a drive letter. Unique partial index while the entity is attached.
- `locator_key` folds Windows-style locators (drive or UNC): case, `/` vs `\`, trailing separators, `\\?\` prefix. POSIX locators keep case. The display `locator` keeps the latest spelling seen.
- `identity` confidence: `path` (default), `verified` (platform id observed and matching), `unresolved` (suspected change, evidence insufficient; never merged), `ended` (known gone; row kept as history, detached from any locator).
- `platform_id` is an optional caller-supplied file id (NTFS file id, inode). It is evidence, not proof: ids can be recycled.
- Availability: `Available`, `Offline` (volume not mounted/reachable), `Missing` (volume reachable, path absent), `Inaccessible` (access denied), `Unknown` (inconclusive/unverified). Offline is never converted to Missing.

## Receipts from Ferrum (FER-261)

`apply_change(FsChange)`, idempotent by `action_id`:

| Kind          | Effect                                                                                                                                                                                                                                                                                                                                                                                                                            |
| ------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Rename / Move | Entity and, for folders, every descendant (boundary-aware: `foo` does not touch `foobar`) get new locators; entity ids, episodes, summary, daily and rank rows are untouched. Parent path and display name follow. Existing occupant of a destination becomes Missing/ended but keeps its history. Moving a folder into its own subtree is rejected. Landing in an excluded root forgets the history and bumps the privacy epoch. |
| Copy          | Nothing is inherited; the source is untouched; destination starts without history.                                                                                                                                                                                                                                                                                                                                                |
| Delete        | Subtree becomes Missing, detached, platform id dropped. History stays visible (consumers filter on availability); a new object at the same path is a new entity.                                                                                                                                                                                                                                                                  |

Structural updates do not depend on the recording switch or privacy epoch: they only modify rows that already exist. A rename is not a visit.

## Lazy validation (FER-262)

`identity_state(id).is_stale(now, max_age)` tells the caller whether to stat. The caller stats with its own timeout/cancel (never per keystroke) and reports `apply_validation(id, probe, now)` with `Present{platform_id}`, `NotFound`, `AccessDenied`, `VolumeUnavailable` or `Indeterminate`. A `Present` probe whose platform id differs from the recorded one ends the identity (`IdentityMismatch`): the entity goes Missing/ended and its locator is freed, so a different file at the same path starts fresh. A receipt (successful navigation/open) counts as a fresh check and sets Available.

Volumes: `mount_volume(volume_id, "E:", now)` / `unmount_volume`. A volume that returns under another letter has its locators rebased (same ids); the previous owner of a letter goes Offline; the new owner gets new entities. Returning volumes are Unknown until validated. `adopt_legacy_volume("C:", volumeId)` re-tags rows recorded when `volume_id` was the drive letter.

Aliases: `link_alias(id, aliasLocator)` after the caller verified a junction/symlink. Receipts via the alias resolve to the target without overwriting its canonical locator; exclusion covering an alias also excludes the target (and its subtree), including when the target is later visited by its real path.

## External changes (FER-263)

`reconcile_external(id, candidate, now)` merges only when all hold: entity attached and currently Missing; same kind; same volume; platform id present on both sides and equal; no other entity claims the id; candidate locator free. Otherwise `Unresolved(reason)` and the entity is flagged `unresolved`; nothing moves. Name/size/time similarity is never evidence. A merged folder carries its descendants along as Unknown.

Recycled identifiers: a Ferrum delete or an identity mismatch drops the platform id, so a recycled id on a new object cannot merge into a dead entity. When a service-derived entity id is already owned by another identity (moved away, ended, or on a volume registered as unmounted) the new object gets a fresh id (`<id>~n`); an unexplained same-id/different-location conflict is still rejected.

## Provider limitations

- Local disk only. Archive members, network history and cloud providers are out of scope; UNC paths are folded like Windows paths but their availability depends entirely on the caller's probes.
- Case sensitivity follows the locator form (drive/UNC: insensitive; POSIX: sensitive). Per-directory case-sensitive folders on Windows (WSL) and case-insensitive POSIX volumes are not detected.
- Short (8.3) names, Unicode normalization differences beyond simple per-character lowercase, and reparse points are not resolved by the store; aliases must be linked by a caller that verified them. Alias resolution is per entity, not per directory subtree.
- File ids: NTFS file ids and inodes can be reused after delete and are not stable across all providers (FAT/exFAT/SMB/FUSE may not offer them). Without an id on both sides external renames stay unresolved. Hard links share an id and make merges ambiguous (refused).
- Cross-volume Move via Ferrum transfers history (Ferrum verified it) but drops the platform id; the new volume must be reported by the caller.
- Restore from Recycle Bin or undo is not a tracked move: the original entity stays Missing/ended and the restored path starts fresh unless Ferrum reports it as a Move from a tracked locator.
- Favorites (`activity_favorites`) are stored by path and are not rewritten by moves.
- Legacy rows with `volume_id` = drive letter have no volume identity until `adopt_legacy_volume` is called; until then drive-letter reuse cannot be detected for them.

## Merge disabled when evidence is insufficient

Identity transfer happens only for Ferrum-verified receipts or platform-id evidence as above. When evidence is missing, ambiguous or contradictory the store leaves entities as they are (`unresolved`), records nothing for the candidate and never rewrites a locator. Suggestions must treat `Missing`, `Offline`, `Unknown`, `Inaccessible` and `unresolved` entities as unverified and validate before open (ACT-05).
