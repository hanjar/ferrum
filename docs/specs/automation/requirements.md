# Automation — Requirements

Status: **Draft for implementation** · Scope: Windows only · Companion documents: [design.md](design.md),
[tasks.md](tasks.md)

## 0. Purpose and reading guide

Automation lets a user run a saved **Sync Profile** without clicking anything, in two ways:

- **Scheduled**: once, daily or weekly at a local time in a chosen IANA time zone.
- **Watch folders**: after file changes, once the watched folders have been quiet for an idle delay.

Each automation picks a **runtime**:

- **While Ferrum is running** (in-app): runs only while a Ferrum window is open.
- **Background**: a per-user headless runner (`ferrum-automation.exe`) that Windows Task Scheduler starts at the
  user's logon. It runs with the user's own identity (same file and SMB access as the user), needs **no
  administrator rights**, does **not** run while the user is signed out, and never wakes the PC from sleep. It is
  **not** a Windows service.

The behaviour comes from an interactive design prototype (six screens: list, editor, review & approve, run
progress, detail with history, dialogs). This document keeps every prototype behaviour unless a requirement below
says otherwise; corrections are marked **(changed from prototype)**. Copy (UI text) quoted here is final unless a
requirement says it is an example.

Conventions:

- Requirement IDs are `<AREA>-<n>`; acceptance criteria are `<AREA>-<n>.<m>`. Implementation packages in
  [tasks.md](tasks.md) cite these IDs.
- EARS keywords: **Ubiquitous** ("The system shall…"), **Event** ("When…"), **State** ("While…"),
  **Unwanted** ("If…, then…"), **Optional** ("Where…").
- Given/When/Then is used where a sequence matters.
- "Host" = the process that executes an automation: the Ferrum app for in-app automations, the runner for
  background automations.
- "Operation" = one planned file operation from the sync engine's comparison (copy, update, create folder,
  delete, held conflict). Counts shown to the user are operations, never raw filesystem events.
- "MUST/SHALL" are binding; "SHOULD" may be deferred only with a note in the package's final report.

## 1. Status inventory (binding)

One StatusChip component renders every state with an icon **and** a label as well as a colour.

| Kind          | Label (example)                          | Meaning                                                         |
| ------------- | ---------------------------------------- | --------------------------------------------------------------- |
| `watching`    | Watching                                 | Monitoring idle; roots have native notifications                |
| `quiet`       | Waiting for quiet · 7 s                  | Changes seen; idle countdown running                            |
| `checking`    | Checking                                 | Fresh comparison after resume / reconnect / overflow / start    |
| `device`      | Waiting for device                       | Root offline; nothing runs, nothing treated as empty            |
| `recheck`     | Needs recheck                            | Watcher overflow; full comparison pending                       |
| `unsupported` | Unsupported                              | Provider has no change notifications                            |
| `scheduled`   | Scheduled                                | Enabled schedule with next absolute time                        |
| `queued`      | Queued                                   | Waiting for the run on overlapping roots                        |
| `pending`     | Pending rerun                            | Changes during a run; one recheck after it                      |
| `running`     | Running 3/6 / Comparing                  | Current run; counts are file operations                         |
| `paused`      | Paused                                   | No new triggers; not the same as stopping a run                 |
| `completed`   | Completed                                | All operations succeeded (6 of 6)                               |
| `partial`     | Partial                                  | e.g. 4 succeeded · 1 failed · 1 not started                     |
| `cancelled`   | Cancelled / Interrupted                  | Stopped by user or by shutdown; completed files not rolled back |
| `skipped`     | Skipped                                  | Missed run with Skip policy, or nothing to do                   |
| `attention`   | Needs attention / Failed / Conflict held | Partial, failed or conflict; new runs held until resolved       |
| `review`      | Needs review                             | Profile root / delete policy / scope or approval changed        |
| `warn`        | Only while open / Runner not running     | Runtime readiness, shown apart from Enabled                     |

**Automation status precedence** (first match wins):
`Needs review → Runner not running (background only) → Running → Queued / Waiting for device → Needs attention →
Paused → trigger state`.
Trigger state is one of `watching | quiet | checking | device | recheck | unsupported | pending` (watch) or
`scheduled | device` (schedule). **(changed from prototype:** the "Runner not running" step is new; it reuses the
`warn` kind.)

Run outcomes use these chips: Completed → `completed`; Partial → `partial`; Conflict held → `attention` "Conflict
held"; Failed → `attention` "Failed" **(new outcome, see RQ-9)**; Cancelled → `cancelled`; Interrupted →
`cancelled` "Interrupted" **(new label)**; Skipped → `skipped`.

## 2. Sync Profile (SP) — prerequisite

There is no named, revisioned sync profile in Ferrum today: Synchronize Dirs is ad hoc between the two panes, and
its plan logic runs in the webview. Automation needs a stored, revisioned profile that a headless process can run.

**SP-1 Named profile.** The system shall store Sync Profiles with: id, name, revision, left root, right root,
sync mode, delete policy, compare mode, filters and a delete safety guard.

- SP-1.1 Given no profiles exist, when the user opens Sync & Backup → Sync profiles, then an empty state explains
  what a profile is and offers "New sync profile" and "Use current panes".
- SP-1.2 When the user chooses "Use current panes", the editor opens prefilled with the active pane as left root
  and the inactive pane as right root.
- SP-1.3 The profile name shall be 1–80 characters, trimmed, without control characters, unique per user
  (case-insensitive). Violations show an inline error and nothing is saved.
- SP-1.4 Roots shall be absolute drive paths (`D:\Photos`) or UNC paths (`\\nas\photos`). Device and NT
  namespaces (`\\?\`, `\\.\`, `\??\`), alternate data streams, `pipe`/`mailslot`/`IPC$` shares and relative paths
  are rejected.
- SP-1.5 If the two roots are the same folder or one is inside the other (after resolving junctions, symlinks,
  case and mapped network drives), then the profile is rejected with the existing message "One folder is inside
  the other; synchronizing them would copy a folder into itself" (or the "same folder" message).

**SP-2 Sync mode.** The profile shall have exactly one mode:

| Mode     | UI label                              | Behaviour                                                                         |
| -------- | ------------------------------------- | --------------------------------------------------------------------------------- |
| `oneWay` | Update left → right (or right → left) | New and changed files are copied from source to target; extras on target are kept |
| `mirror` | Mirror left → right (or right → left) | As `oneWay`, and extras on the target are deleted through the delete policy       |
| `twoWay` | Two-way                               | Baseline-based; only the side proven changed may overwrite; conflicts are held    |

- SP-2.1 `oneWay` and `mirror` shall classify entries exactly like today's one-directional Synchronize Dirs
  (parity is proven by shared contract fixtures, see design §5.2).
- SP-2.2 `twoWay` shall classify exactly like today's "Both directions" mode, including baseline trust rules: no
  winner is chosen from timestamps; without a trusted baseline every difference is a conflict.
- SP-2.3 Where `twoWay` has "Propagate deletions" enabled (default **off**), when the trusted baseline proves a
  file existed on both sides, is now missing on one side and is unchanged on the other side, the engine shall plan
  a delete on the other side through the delete policy. Every other missing-file case stays a conflict.
- SP-2.4 The editor shall show one sentence per mode explaining overwrites and deletions (e.g. "Left always wins;
  right becomes an exact copy").

**SP-3 Delete policy.** Every profile that can delete (mirror, two-way with propagation) shall choose one:

- `recycleBin` (default) — deleted files go to the Recycle Bin of the side they were deleted on.
- `versioning` — deleted **and overwritten** files are moved to a versioning folder:
  `<versioning root>\<Left|Right>\<yyyy-MM-dd HHmmss> #<run id>\<relative path>`.
- `permanent` — deleted files are removed without the Recycle Bin.

Acceptance:

- SP-3.1 If a side that can receive deletes is not on a local fixed drive (network share, removable drive, or a
  volume without a Recycle Bin), then `recycleBin` shall be refused for that profile with "Recycle Bin isn't
  available on <root> — choose a versioning folder or permanent delete". Ferrum never lets the Recycle Bin request
  silently become a permanent delete.
- SP-3.2 The versioning root shall be an absolute local or UNC folder that is not inside either sync root and does
  not contain either root. It is created on first use; if it is unavailable at run time, the run fails before any
  operation (RQ-9) and nothing is deleted or overwritten.
- SP-3.3 When the user saves a profile whose delete policy is `permanent` (new profile, or policy changed to
  `permanent`), the backend shall show a **native** Windows confirmation (SE-2) before saving. Cancel = nothing
  saved.
- SP-3.4 Overwrites under `recycleBin` or `permanent` replace the target in place (existing copy-engine
  behaviour); only `versioning` keeps the previous version of overwritten files. The review screen states this.

**SP-4 Compare mode and filters.**

- SP-4.1 Compare mode is "Size and date" (default, mtime tolerance 2 s) or "Content".
- SP-4.2 Filters are an exclude list (default empty) and an optional include list, each ≤ 64 patterns of ≤ 260
  characters. Pattern syntax: `*` (within one path component), `?`, `**` (any number of components), matched
  case-insensitively against the path relative to the root with `\` separators. A pattern ending in `\` matches
  folders only.
- SP-4.3 Excluded entries are invisible to the comparison on **both** sides: never copied, never overwritten,
  never deleted (including by `mirror`). Excluded folders are not descended.
- SP-4.4 Always excluded, not editable: Ferrum copy staging files (`.*.ferrum-stage-*`, `.*.ferrum-backup-*`),
  `$RECYCLE.BIN` and `System Volume Information` at a volume root.
- SP-4.5 The editor shows a live example: "Excluded: 3 patterns · e.g. `node_modules\`, `*.tmp`".

**SP-5 Delete safety guard.** (not in prototype; protects unattended runs)

- SP-5.1 Each profile has a guard, on by default: a run that would delete (or move to versioning) more than
  `max(50 items, 20 % of the target's compared entries)` is **held before any operation**: outcome Failed, reason
  "Would delete 812 items (41 % of E:\Backups) — over the safety limit. Review in Sync.", automation → Needs
  attention. Thresholds are editable per profile (count 1–100 000, percent 1–100) and the guard can be turned off
  only together with a native confirmation.
- SP-5.2 If a source root is empty (0 entries after filters) while the target has entries and the plan contains
  deletes, then the run is held the same way ("Source folder is empty — nothing deleted"), regardless of the guard
  thresholds.

**SP-6 Revisions.**

- SP-6.1 The revision starts at 1 and increases by exactly 1 on every save that changes a **policy field**:
  either root, root identity, mode, source side, two-way deletion propagation, delete policy, versioning root,
  compare mode, filters, guard. Renaming a profile does not change the revision.
- SP-6.2 Saves use optimistic concurrency: the editor sends the revision it loaded; a mismatch returns `stale`
  and the editor reloads with "This profile was changed elsewhere. Review the current version."
- SP-6.3 The system shall keep, per revision, which policy fields changed with their old and new display values,
  so the review screen can show "Right root: E:\Backups\Projects → F:\Backups\Projects". At least the last 50
  revisions are kept.
- SP-6.4 When a profile revision changes, every automation using it shall become **Needs review**, its queued
  runs are removed, its watch state resets to Watching, pending reruns are cleared, and an event "Profile changed
  to rev 4 (delete policy) — held until you review" is logged per automation. A run already in progress finishes
  under the revision it started with.

**SP-7 Root identity.** (not in prototype; guards against "the drive letter now points to another disk")

- SP-7.1 When a profile is saved, the system shall record for each local root the volume serial number and the
  root folder's file ID; for a UNC root, the normalized `\\host\share` prefix.
- SP-7.2 If, at run time, a local root exists but its volume serial or folder file ID differs from the recorded
  one, then the root is treated as **unavailable** (`device`, "E:\ is a different drive than when the profile was
  saved") — never as an empty or replaced folder. Re-saving the profile in the editor (with the new identity shown)
  creates a new revision.
- SP-7.3 A root folder that was deleted and re-created gets a new file ID and therefore also needs a profile save.
  The editor explains this case.

**SP-8 Readiness.** The system shall compute readiness per profile: Ready, or Not ready with one reason (root
missing, identity mismatch, Recycle Bin unavailable, versioning root invalid, profile invalid). Automations can only
pick Ready profiles (AP-2.3). Readiness is computed by the backend, never by the webview.

**SP-9 Editor (no prototype; follows existing Ferrum patterns).**

- SP-9.1 The Sync profiles page lives in the sidebar under a new "Sync & Backup" section with "Sync profiles" and
  "Automation" entries, rendered as a full page like Activity.
- SP-9.2 The editor is one scrolling form with sections: Name · Folders (left, right, each with path input,
  suggestions from the existing path suggester, "Browse…", "Use active pane", volume/kind caption such as "Local
  disk · NTFS") · Direction (segmented control: Update / Mirror / Two-way, plus a "Left → Right / Right → Left"
  toggle for Update and Mirror) · Deletions (radio cards for the three policies, versioning folder field) ·
  Compare (Size and date / Content) · Filters (two textareas, one pattern per line) · Safety (guard switch and
  thresholds) · Readiness (check list as in the automation editor).
- SP-9.3 Footer: Cancel, Save. Save is disabled while there are validation errors; errors are listed next to the
  button as in the automation editor.
- SP-9.4 From a profile the user can "Preview sync" (dry-run comparison and the effect summary, read-only) and
  "Run now" (manual run through the same engine, with the existing overwrite and delete confirmations in the
  webview; the permanent-delete confirmation is native).
- SP-9.5 Deleting a profile used by an automation is refused: "Used by 2 automations — delete or change them
  first."

## 3. Watch folders (WA)

**WA-1 Watch lifecycle.** The trigger state machine shall follow
`Watching → Changes detected → Waiting for quiet (each event resets the idle delay) → Queued → Running → Watching`.

- WA-1.1 Given idle delay 10 s, when change events arrive at t=0 and t=6, then exactly one run is queued at t=16
  (a burst counts as one run).
- WA-1.2 The quiet countdown shows remaining seconds ("Waiting for quiet · 7 s") and the sub-line "Changes
  detected — new changes reset the countdown".
- WA-1.3 Idle delay is an integer 2–3600 s (stepper and text input, clamped).
- WA-1.4 **(addition)** While events keep arriving, after `max wait` (default 15 min, range 1–1440 min, see open
  question Q3) since the first event of the burst, one run is queued anyway; the countdown restarts after it.

**WA-2 Watched roots and triggers.**

- WA-2.1 The user picks which roots are watched (left, right); only roots with native change notifications can be
  selected. At least one selectable watched root is required.
- WA-2.2 Triggers: "File & folder changes" (new, changed, renamed or deleted items start the quiet countdown) and
  "Location becomes available" (when a watched drive or share reconnects: compare first, then run only if needed).
  At least one trigger is required.
- WA-2.3 Native notifications are supported on local fixed and removable drives (NTFS, ReFS, exFAT, FAT32).
  Network roots (UNC and mapped drives) are **Unsupported** in this version (open question Q2). Unsupported roots
  show "No change notifications on this provider — can't be watched". There is **no polling fallback** for change
  detection.
- WA-2.4 File notifications only trigger a fresh comparison by the sync engine; they are never the list of what
  gets copied. The editor shows this sentence.
- WA-2.5 Events on excluded paths (SP-4.3, SP-4.4) are ignored and do not start or reset the countdown.

**WA-3 Changes during a run.**

- WA-3.1 When events arrive for a watched root while a run holds a lease on overlapping roots (any process), the
  automation shall set a single **pending rerun** flag (event "5 file events during the active run → 1 pending
  rerun"); further events merge into it ("merged into the pending rerun").
- WA-3.2 When that run finishes, the engine shall compare again before running ("Pending rerun → recompare
  folders before running"); a run is queued only if differences exist.
- WA-3.3 Events that arrive while a run for the same profile is already queued are merged into it ("merged into
  the queued run").
- WA-3.4 While the trigger state is `checking` or `recheck`, events are noted ("the comparison will include them")
  and do not start a countdown.

**WA-4 Self-generated changes.**

- WA-4.1 Changes written by a run shall never trigger that automation (or any other automation on overlapping
  roots) into a new run. Mechanism: events correlated with the run's written paths and run window are ignored, and
  any remaining doubt is resolved by the post-run recompare (WA-3.2).
- WA-4.2 After a run with successful operations, every enabled watch automation on overlapping roots logs
  "Ignored 6 changes written by run #1042 (self-generated) — no rerun" when the recompare finds nothing to do.

**WA-5 Offline roots.**

- WA-5.1 If a profile root becomes unavailable (unplugged, network down, identity mismatch), then the state becomes
  **Waiting for device** with sub-line "E:\Backups\Projects not connected — no sync". It is never compared as if
  the folder were empty, and nothing is deleted.
- WA-5.2 When the root becomes available again and "Location becomes available" is on, the state becomes
  **Checking** ("E:\ available again → fresh comparison before any run"); a run is queued only if differences
  exist. With the trigger off, the state returns to Watching.
- WA-5.3 Availability of an offline root is detected from volume arrival notifications for local drives and, for
  UNC roots, by an existence probe of the root at most every 30 s while in `device`. The probe never lists folder
  contents.

**WA-6 Overflow.** If the watcher reports an overflow or lost events, then the state becomes **Needs recheck**
("Watcher missed events — full comparison when safe"); a full comparison runs as soon as no run is active on
overlapping roots; differences queue one run.

**WA-7 Resume and start.**

- WA-7.1 When monitoring is resumed (Enabled switched on), the state becomes Checking ("Monitoring resumed — fresh
  comparison to catch changes made while paused").
- WA-7.2 When a host starts (Ferrum opened for in-app automations, runner started at logon for background
  automations) and when the PC resumes from sleep, every enabled watch automation it hosts runs one fresh comparison
  (Checking) before going to Watching.
- WA-7.3 Where the FerrumIndexer journal can prove that neither root changed since the last fully verified run
  (design §11), the Checking comparison may complete without reading file contents (IX-1).

**WA-8 Countdown preview.** The editor offers "Simulate a change"; it starts a local preview countdown with the
chosen idle delay ("Any new change resets the countdown to 10 s"), ends with "Preview only — 3 changes batched into
one run", and never runs or queues anything.

## 4. Schedule (SC)

**SC-1 Frequency.** Once (date from today up to 13 days ahead and time), Daily (time), Weekly (one or more
weekdays and time). Time is `HH:MM` 24-hour.

- SC-1.1 Validation errors: "Pick at least one weekday", "Enter a valid time", "That time has already passed — pick
  a later date or time".
- SC-1.2 The editor shows the next 3 absolute runs, each with date, local time and zone abbreviation, the relative
  time ("in 9 h 15 min"), and, when the schedule zone differs from the system zone, the equivalent system-local
  time ("= Tue 20:00 WIB").
- SC-1.3 After a Once automation has fired (or its time was handled as missed), it stays enabled with status
  Scheduled · "No upcoming run".

**SC-2 Time zones.** Any IANA zone from the bundled time zone database; default = the system zone.

- SC-2.1 A local time that does not exist (DST gap) is skipped for that day, recorded as a Skipped history entry
  "Skipped — 02:30 doesn't exist on Sun 29 Mar in Europe/Amsterdam (daylight saving)".
- SC-2.2 A local time that occurs twice (DST fold) runs once, at the first occurrence.
- SC-2.3 The editor's DST note: "Daylight saving: a local time that doesn't exist is skipped; a local time that
  happens twice runs once." For zones without DST: "<zone> has no daylight-saving changes. For zones that do: a
  nonexistent local time is skipped, a repeated one runs once."

**SC-3 Missed runs.** An occurrence is **missed** when its host did not fire it within 120 s after its time (Ferrum
closed for in-app, runner not running, PC asleep or off, user signed out).

- SC-3.1 Policy "Skip it (default)": one Skipped history entry per detection, aggregating all missed occurrences
  ("Missed 3 scheduled runs — Ferrum was closed. Policy: skip; waits for the next time."). Nothing piles up.
- SC-3.2 Policy "Run once when available again": the same Skipped entry plus **one** catch-up run queued (trigger
  "Missed run · catch-up"), never one per missed time.
- SC-3.3 Missed runs are detected when the host starts, when the PC resumes from sleep and when the system clock
  or time zone changes.
- SC-3.4 An occurrence is never fired twice, including after the clock moves backwards.

**SC-4 Device offline at schedule time.** If a root is unavailable when an occurrence fires, then the run is queued
and held as Waiting for device ("Queued run held — E:\ not connected"); it starts when the device returns. It never
runs against an empty or missing folder.

## 5. Runtime and background runner (RT)

**RT-1 Runtime chooser.** The editor offers two radio cards:

- "While Ferrum is running" — chip Available. Description (watch): "Watches while Ferrum is open. Changes made
  while it's closed are caught by a fresh comparison on next start." (schedule): "Runs at the scheduled time if
  Ferrum is open. Won't run while Ferrum is closed, the PC sleeps or you're signed out."
- "In the background" **(changed from prototype "Background service (system scheduler)")** — chip Unavailable /
  Needs setup / Ready. Description (watch): "Keeps watching while Ferrum is closed, as long as you're signed in to
  Windows. Doesn't wake a sleeping PC." (schedule): "Runs even when Ferrum is closed, as long as you're signed in to
  Windows. Doesn't wake a sleeping PC or run while you're signed out."
- RT-1.1 Disabled reasons: Unavailable — "The background runner isn't included in this Ferrum build." Needs setup
  — "Set up the Ferrum background runner first. It starts when you sign in to Windows and doesn't need
  administrator rights." **(changed from prototype "needs administrator rights")**, with a "Set up runner…"
  button.
- RT-1.2 Rows and detail show the runtime note: "Runs only while Ferrum is open" / "Watches only while Ferrum is
  open" (amber, `warn`) or "Background · runs while Ferrum is closed" (neutral) or "Background runner not running
  — won't run" (amber).

**RT-2 Runner setup.**

- RT-2.1 When the user chooses "Set up runner…", the backend shows a native confirmation (SE-2) explaining:
  "Ferrum will add a task to Windows Task Scheduler that starts the Ferrum automation runner when you sign in.
  It runs as you, without administrator rights, only while you're signed in, and never wakes the PC." On OK it
  registers the per-user logon task and starts the runner immediately.
- RT-2.2 Setup shall not require elevation. If Windows refuses registration, the capability stays Needs setup with
  the refusal reason, and nothing else changes.
- RT-2.3 Capability is computed by the backend: **Unavailable** (runner binary missing), **Needs setup** (task
  missing, disabled, or pointing to another executable path), **Ready** (task registered for this user, enabled,
  pointing to the runner next to the running Ferrum).
- RT-2.4 Settings shows the runner state (Ready / Needs setup / Unavailable, running or not, version) with "Set up
  runner…" and "Remove runner". Removing unregisters the task, stops the runner and pauses every background
  automation (event "Paused — background runner removed").

**RT-3 Runner behaviour.**

- RT-3.1 The runner shall run under the signed-in user's token at normal integrity, with no window except the
  tray icon (NT-3).
- RT-3.2 The runner shall not run while the user is signed out and shall not wake the PC (the task is registered
  without "wake to run").
- RT-3.3 Exactly one runner per user and data profile runs at a time; a second start exits immediately.
- RT-3.4 When there is no enabled background automation and nothing is queued or running for 10 minutes, the
  runner exits. Ferrum starts it again when a background automation is enabled; the logon task starts it at the
  next sign-in.
- RT-3.5 The runner only executes automations whose runtime is Background; in-app automations are executed only
  by the Ferrum app.
- RT-3.6 When the Ferrum app is open and a background automation is enabled but the runner is not running, the app
  shall start the runner (same user, no elevation) and show "Runner not running" until it reports in. The app
  never executes background automations itself (open question Q8).

**RT-4 In-app host.** Exactly one Ferrum app instance per user hosts in-app automations; other open instances show
the same data and send controls, but do not execute.

## 6. Review & approval (AP)

**AP-1 Flow.** New or edited automations go: Editor → "Continue to review" → Review → "Run test" → "Approve &
enable" (new) / "Approve & save" (edit) / "Approve & resume" (re-review after a profile change).

**AP-2 Editor.**

- AP-2.1 Fields: Name (1–80), Sync profile (radio cards: name, rev, roots with arrow, direction, delete policy,
  readiness, "Edit sync profile ↗"), trigger fields (SC-1 or WA-2), runtime (RT-1), "If a run is missed"
  (schedule), Advanced policies (collapsed: overlapping runs, conflicts, transient-error retries 0–3 with default
  2, offline location, profile changes; optional conditions OP-1).
- AP-2.2 A Readiness block lists checks with ok / warn / fail icons (profile ready, roots reachable, notification
  support, runtime readiness).
- AP-2.3 Not-ready profiles are shown disabled with their reason. With no profiles: "Automations need a saved sync
  profile" and "Create a sync profile ↗".
- AP-2.4 "Continue to review" is disabled while validation errors exist; the errors are listed.

**AP-3 Review screen.** Sections: What will run (profile, rev, source/target with kind captions, direction,
"Edit sync profile ↗"); Profile changes since the approved revision (field, from → to) when re-reviewing; Effect on
your files (updates, new files and folders, deletions or "0 deletions — Delete propagation is off", total
operations in today's comparison); Trigger (with next runs for schedules); Runtime (with "Enabled doesn't mean
always running…" for in-app); Policies (missed run or burst handling; DST or self-generated changes; overlapping
runs; conflicts; errors; offline location; stopping a run); Test run; approval footer.

**AP-4 Test run.**

- AP-4.1 "Run test" validates without changing files: profile valid at its current revision; left readable; target
  side(s) writable (checked with an access check, no file is written); comparison summary
  ("Comparison: 6 operations (2 update, 3 new, 1 folder, 0 delete)"); conflicts ("1 conflict found — would be held
  for review, never overwritten" as warn); delete guard result; runtime readiness; native notifications per watched
  root.
- AP-4.2 Steps appear one by one with spinner / ok / warn / fail. The first fail stops the test and its text is
  shown ("E:\Backups\Projects is unavailable — not treated as empty. Reconnect and test again.").
- AP-4.3 A passed test shows "Test passed — no files were changed. The comparison found 6 operations; the real
  run recompares first."
- AP-4.4 The approve button is enabled only after a passed test of the exact policy being approved; the backend
  enforces the same rule (SE-3.2).

**AP-5 Approval.** When the user clicks Approve, the backend shows a native confirmation (SE-2) summarising the
policy; on OK it stores the approval (policy hash, profile revision), enables the automation and returns to the
list (schedule) or detail (watch) with the row highlighted for 5 s and toast "<name> enabled — background runs
won't ask again for rev 3". Event: "Approved "Projects backup" rev 3 with the reviewed policies · enabled" (or
"Re-approved").

**AP-6 Needs review.** If the profile revision, the profile policy or the automation policy no longer matches the
approval, then the automation is **Needs review** ("Profile changed (rev 3 → 4) · held"), triggers are logged as
"held (needs review)", Run now shows "Review the profile change before running", and Review opens the review screen
with the changes listed.

## 7. Run & queue (RQ)

**RQ-1 Pipeline.** Every run, whatever the trigger: gate checks (SE-4) → fresh comparison → plan → safety guards
(SP-5) → execute operations → verify and settle the two-way baseline → record history.

**RQ-2 One run per folder set.** At most one run (automation, manual profile run or manual Synchronize Dirs) may be
active on overlapping roots across all Ferrum processes of the user. Roots overlap when equal or nested after
canonicalization (case-insensitive, junctions resolved, mapped drives resolved to UNC). The versioning root counts
as a root of its run.

**RQ-3 Queue.**

- RQ-3.1 Triggers for a profile that already has a queued entry are merged into it (merge count and reasons kept;
  "Merged into the pending run for these folders — no duplicate job").
- RQ-3.2 A queued entry waits while another run uses overlapping roots ("Waits for "Nightly photos to NAS" — same
  folders"), while a root is offline (`device`) or while an optional condition is unmet (OP-1).
- RQ-3.3 Queued entries start in FIFO order within each host when they can.
- RQ-3.4 The queue survives a restart of its host (stored), except entries removed by pause or profile change.

**RQ-4 Run now.** Run now enqueues a run (trigger "Run now"). Toasts: queued / waiting / merged / device as in the
prototype. If the automation needs review or attention, Run now is dimmed and shows the reason instead of
enqueueing.

**RQ-5 Progress.** While running, the detail shows phase ("Comparing folders…", "Syncing 3 of 6", "Stopping after
the current file…"), a progress bar by operations, counts "3 succeeded · 0 failed · 3 remaining", and a per-operation
list (Done / In progress / Failed / Not started / Held / Pending). Lists longer than 200 rows are paged.

**RQ-6 Conflicts.** Conflicts are held, never resolved unattended: no newest-wins, no overwrite. A run with held
conflicts ends as **Conflict held** → Needs attention ("1 conflict held for review"). History offers "Review in
Sync ↗" (opens Synchronize Dirs with the profile's roots) and Recheck.

**RQ-7 Errors and retry.**

- RQ-7.1 Transient errors (sharing or lock violation, network timeout, network name deleted, network busy) are
  retried up to the configured count (0–3, default 2) with backoff 2 s, 10 s, 30 s, each retry after re-checking
  that the source and target still match the plan.
- RQ-7.2 Systemic errors (access denied, sign-in failure, disk full, write protected) stop the run at once:
  remaining operations become Not started, outcome Partial (or Failed if none succeeded), automation → Needs
  attention; note "Not retried automatically — permission errors need you. Fix access, then Recheck."
- RQ-7.3 Per-item errors (file vanished, name invalid on target, path too long, file too large for the target file
  system) fail that operation and the run continues.
- RQ-7.4 If a root disappears during a run, the run stops after the current file; outcome **Interrupted**
  ("E:\ disconnected during the run — completed files stay"); the automation goes to Waiting for device and does
  **not** enter Needs attention; on reconnect a fresh comparison decides.
- RQ-7.5 Just before each operation the engine re-reads the facts of source and target; if either changed since the
  plan, the operation is skipped as "Changed during the run" and a pending recheck is set.

**RQ-8 Stop and pause.**

- RQ-8.1 Stop ("Stop the current run?"): the file in progress finishes or is rolled back, remaining operations are
  Not started, outcome Cancelled, note "Stopped by you. 3 completed files stay as synced — no automatic rollback."
  Monitoring stays on.
- RQ-8.2 Pausing an automation with a run in progress asks: "Pause & let it finish" (default), "Pause & stop
  current run", Cancel. Pausing removes the automation's queued entries and pending rerun.
- RQ-8.3 Pause never needs confirmation beyond this dialog; Resume requires the native confirmation (SE-2.1).

**RQ-9 Outcomes.** Completed · Partial · Conflict held · Failed **(new: the run could not start or stopped before
any operation succeeded because of an error or a safety guard)** · Cancelled · Interrupted **(new label)** ·
Skipped (missed run, DST gap, or "Fresh comparison found nothing to do").

- RQ-9.1 Partial, Conflict held and Failed set **Needs attention** with the run id; new runs of that automation are
  held ("1 failed · 1 not started · new runs held") until resolved.
- RQ-9.2 Interrupted and Cancelled do not set Needs attention.

**RQ-10 Recheck and retry.**

- RQ-10.1 Recheck runs a fresh, read-only comparison and reports "Fresh comparison: 4 earlier operations are in
  sync and won't be repeated · 2 operations remaining."
- RQ-10.2 "Retry remaining N" (shown only after a Recheck that found N > 0 and the run was not a conflict) queues a
  run with `retryOf`; it executes a fresh plan, so successful operations are never repeated.
- RQ-10.3 A Recheck that finds 0 remaining clears Needs attention ("Resolved — folders are in sync").
- RQ-10.4 A successful retry marks the original run "Resolved by run #1043" and clears Needs attention.
- RQ-10.5 For a conflict: after the user resolved it in Sync, Recheck clears attention ("Resolved in Sync by you");
  otherwise it reports "the file still changed on both sides. Decide in Sync — Ferrum won't choose a version."

## 8. Monitoring & history (MH)

- MH-1 The Automation page has tabs "Automations (n)" and "Run history" (badge "2 need attention" in red).
- MH-2 The automation list row shows: Enabled switch, name and profile with approved rev, trigger and runtime note,
  status chip with sub-line, last result chip with time and counts, actions Run now / Edit / Review or Resolve.
  Rows are ≥ 42 px.
- MH-3 An attention banner above the list: "1 automation needs you — Nightly photos to NAS — new runs are held until
  reviewed." with a link to the filtered history.
- MH-4 A queue strip shows the running run, queued entries (with reason and merge count) and pending reruns.
- MH-5 Detail page: status with explanation, lifecycle pipeline (watch: Watching → Changes detected → Waiting for
  quiet → Queued → Running; schedule: Scheduled → Queued → Running → Finished), quiet countdown bar, current run,
  waiting entries, trigger and roots, runtime, profile summary, recent runs (4), events (14 most recent), Enabled
  switch, Pause/Resume, Run now, Stop current run, Edit, Review changes / Resolve held run.
- MH-6 Run history: columns Run · Automation · Trigger · Started → ended · Result · Succeeded · Failed · Not
  started; filter All runs / Needs attention; expand a row to see operations, error text, note, Recheck / Retry /
  Review in Sync / Open automation. Footnote: "Counts are file operations from the engine's comparison — not
  filesystem events."
- MH-7 Event log entries are time-stamped with kinds ok / warn / bad / mute / info; self-generated changes are shown
  as ignored (mute). At least the last 200 events per automation are kept.
- MH-8 History keeps every run with start and end, trigger, outcome, per-operation results (first 5 000 operations
  stored, the rest counted), error text and notes.
- MH-9 The sidebar "Automation" entry shows a badge with the number of automations needing review or attention.
- MH-10 Views update within 2 s of a change made by the other process (runner or another app instance).

## 9. Notifications & tray (NT) — background runner

- NT-1 The runner shall show a Windows notification when a background run ends **Partial**, **Failed** or
  **Conflict held** (title "<automation name>: needs attention", body e.g. "Run #1041 partial — 1 failed, 1 not
  started. New runs are held."), and when a background automation cannot run because it **needs review**.
- NT-2 Notifications are rate-limited: at most one per automation per 15 minutes for the same outcome kind; no
  notification for Completed, Skipped, Cancelled or Interrupted.
- NT-3 The runner shows a tray icon while it runs, with states Idle, Running and Needs attention (badge). Tooltip:
  "Ferrum automation — 2 watching · 1 needs attention". Menu: Open Ferrum Automation · Pause all background
  automations · Quit runner (automations stop until the next sign-in).
- NT-4 Clicking a notification or "Open Ferrum Automation" opens Ferrum on the automation (or the Automation page);
  if Ferrum is not running it is started.
- NT-5 Notification and tray text are built by the runner from stored data, bounded to 256 characters per line,
  with control characters removed.

## 10. Security (SE)

Threat model summary (details in design §10): the webview may be compromised; same-user native processes are out of
scope; nothing in Automation requires or obtains elevation.

- **SE-1 Webview cannot authorize.** Creating, changing, approving, resuming (enabling) an automation, setting up
  the runner, saving a profile with permanent delete and turning off the delete guard shall each require a native
  confirmation shown by the backend. No IPC command accepts a "confirmed" or "approved" flag from the webview.
- **SE-2 Native confirmation.** A Windows `MessageBoxW` owned by the Ferrum window, text built by the backend from
  validated, stored data (never webview-supplied display text), Cancel as default button, warning icon, bounded
  length (≤ 2 000 characters), as in the Hotlist command confirmation.
  - SE-2.1 Enabling (Resume) shows "Resume "<name>"? It will run "<profile>" rev 3 (<left> → <right>, <mode>,
    <delete policy>) <trigger>, <runtime>."
  - SE-2.2 Permanent delete text starts with "WARNING: deleted files are removed permanently, without the Recycle
    Bin."
- **SE-3 Approval binding.**
  - SE-3.1 An approval stores the policy hash (SHA-256 over the profile policy and automation policy, design §8.3),
    the hash version, profile id and revision, and a MAC keyed with a per-user secret protected by Windows DPAPI.
  - SE-3.2 The backend accepts an approval only if a passed test for the same policy hash happened in the same
    process within the last 15 minutes, and only if the policy hash recomputed **after** the native confirmation
    equals the one shown; otherwise `stale` and nothing is stored.
- **SE-4 Gate before every run.** Every host shall, immediately before every run (including retries, catch-up and
  pending reruns), verify: automation enabled; runtime matches the host; approval present, hash version supported,
  recomputed policy hash equal, profile revision equal, MAC valid; no attention hold (except the resolving retry);
  roots available with matching identity; versioning root valid; lease acquired. Any failure → no file operation,
  status Needs review / Waiting for device / Needs attention as appropriate, event logged.
- **SE-5 Storage isolation.** Profiles, automations, approvals and history live in a dedicated store under the
  user's local (non-roaming) application data. They shall not be readable or writable through `load_prefs` /
  `save_prefs` or any generic file command; only the dedicated commands of design §7 touch them.
- **SE-6 Runner input.** The runner exposes no IPC endpoint (no pipe, socket or window message that carries
  commands). It reads controls only from the shared store, and controls can only pause, stop, run now, recheck,
  retry, reload or restart — never create, change, approve or enable anything.
- **SE-7 IPC validation.** Every command validates ids (UUID v4 text), lengths, enums, numeric ranges, time and
  zone syntax and paths (SP-1.4) in Rust; errors cross IPC as `{ code, message }` without paths.
- **SE-8 No elevation.** No automation path shall request UAC, run elevated, install a service or write outside
  the user's profile, the chosen sync roots and the versioning root.
- **SE-9 Versioning writes** are created without following junctions or symlinks planted under the versioning root.

## 11. Lifecycle (LC)

- LC-1 Uninstalling Ferrum shall stop running runners of all users and delete their logon tasks. If that fails for
  a user, the runner unregisters its own task and exits the next time it starts and finds Ferrum missing.
- LC-2 Updating Ferrum while a runner is running shall stop the runner before files are replaced; an active run
  stops after the current file with outcome Interrupted ("Stopped because Ferrum was updated or closed"); after the
  update the runner starts again (at the next logon, or immediately when Ferrum starts).
- LC-3 The app and the runner shall record their version and store schema version. A process older than the store
  schema shall not write to the store and shall exit (runner) or show "Ferrum was updated — restart Ferrum" (app).
- LC-4 When the app starts and the running runner's version differs from the app's, the app shall ask the runner to
  restart when idle and start the matching runner.
- LC-5 Store migrations are forward-only, run by the newest version under an exclusive lock, and keep a backup of
  the previous store file until the migration succeeds.
- LC-6 A crash or kill of any host leaves no lease held longer than 30 s, no run stuck as Running (it becomes
  Interrupted on the next start) and no partially written file published (copy staging).
- LC-7 Windows sign-out or shutdown: the runner stops like LC-2 (outcome Interrupted).

## 12. Fast compare synergy (IX)

- IX-1 Where the user enabled "Sync cepat dengan journal indexer" (`syncJournalSkip`) and the FerrumIndexer
  answers, profile runs, test runs, Checking comparisons and Rechecks by content shall use the same incremental
  content-skip rules as manual sync (FER-477). Anything less than a complete journal answer falls back to the
  ordinary comparison; correctness never depends on the indexer.
- IX-2 The runner shall not require the indexer service and shall not change its mode.

## 13. Optional features (OP) — may ship after P4

- OP-1 **Conditions** per automation, off by default: "Don't run on battery power" and "Don't run on metered
  networks" (applies only when a root is a network path). An unmet condition keeps the queued entry waiting with
  "Waits for AC power" / "Waits for an unmetered network".
- OP-2 **History retention**: keep runs for 90 days or the last 500 runs per automation (whichever keeps more),
  events for 30 days; editable in Settings. Retention never deletes the run that holds an attention.
- OP-3 **Versioning retention**: delete versioning entries older than N days (off by default). Requires its own
  security review before enabling.

## 14. Non-goals

- A Windows service, SYSTEM-level or machine-wide runner; running while the user is signed out; waking the PC.
- Polling for change detection; watching network shares in this version.
- Cloud providers through their APIs (OneDrive etc.); roots are filesystem paths only.
- Unattended conflict resolution (newest-wins or similar).
- Cron expressions, intervals shorter than daily, or multiple times per day (add later as a new trigger).
- Pre/post scripts or launching programs from automations.
- A "Versions" browser page for the versioning folder (sidebar entry reserved, not built).
- Migrating the ad-hoc pane Synchronize Dirs flow to the Rust engine (it keeps its TypeScript plan logic; parity is
  enforced by fixtures). Only profile runs use the Rust engine.
- Linux and macOS.
- Email, webhook or mobile notifications.

## 15. Owner decisions

All questions are decided; see [design.md §14](design.md#14-owner-decisions) (every recommended default was accepted). Requirements that
depend on them: WA-1.4 (Q3), WA-2.3 (Q2), SP-5 (Q4), SP-2.3 (Q5), RT-2 (Q1), RT-3.6 (Q8).
