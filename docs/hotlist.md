# Directory Hotlist (FER-458)

Ctrl+D (and the ☆▾ button at the end of each address bar) opens a nested popup of favourite folders,
modelled on Total Commander. **Configure…** edits the tree; **Settings › Hotlist** chooses where the
file lives.

## Popup

- Accelerator column on the left: a leading number (`"2. Project"` → `2`) or the letter after `&`
  (`"&Telegram"` → `T`). A number is the shown key; the `&` letter still works. `"4. &Charlie"` answers to
  both `4` and `C`. Several matches cycle on repeated presses.
- ▸ marks a submenu (→ or Enter opens, ← or Esc steps back), ▶ marks a command item, ✓ marks the folder
  the active pane is in.
- Footer: **Add current folder** (`+`), **Remove current folder** (`-`), **Configure…** (`,`).
- A `dir` item with a **target path** also opens that path in the other pane.
- A folder (path or target path) can never be a device / NT namespace path: `\\.\…`, `\\?\…`
  other than the local long form `\\?\X:\…`, `\??\…`, `\Device\…` or anything with
  `\Device\Mup\` (FER-467 H1). Some of these reach a network host without looking like UNC
  (`\\.\UNC\host`, `\\?\GLOBALROOT\Device\Mup\host`). They are refused on load, save and in
  Configure (with a hint), and skipped on import with the warning `device_path`.
- A `net` tag marks a folder (or its target path) on a network share; see
  [Network folders](#network-folders-fer-467-r8).

## File: `hotlist.json` (schema v2)

```json
{
  "version": 2,
  "source": "free-form note",
  "items": [
    {
      "type": "dir",
      "title": "&Downloads",
      "path": "C:\\Users\\me\\Downloads",
      "targetPath": "D:\\x"
    },
    { "type": "submenu", "title": "&1. Work", "items": [] },
    { "type": "separator" },
    { "type": "command", "title": "kando", "command": "C:\\tools\\kando.exe", "args": ["--x"] }
  ]
}
```

Limits (checked by the backend on load, save and import; the frontend mirrors them): 8 levels of
submenus, 2000 items, titles ≤ 256 characters, paths ≤ 4096, ≤ 32 arguments, file ≤ 2 MiB. Anything
outside the limits rejects the whole document.

The old flat list in `localStorage` (`ferrum.hotlist.v1`) is migrated once, on the first start with no
hotlist file, and removed only after the new file was written.

## Storage rules

- Default location: the app config dir (`%APPDATA%\dev.ferrum.app\hotlist.json`; under
  `FERRUM_PROFILE_DIR` it follows the test profile).
- **Settings › Hotlist** can point it elsewhere (full local `.json` path; no UNC/device paths, no `..`),
  e.g. into a cloud drive. The location itself is stored in `hotlist-location.json` next to `config.json`.
  - FER-467 R7: the location is also checked on the filesystem (`path_guard::check_local_location`),
    when it is chosen and again every time it is used. Refused, with a reason in Settings: a drive
    letter mapped to a network share (`GetDriveTypeW` = remote, checked before anything else is
    touched), a junction/symlink anywhere in the path (name-surrogate reparse tags; cloud sync
    cloud-file folders are not links and stay allowed), a folder whose real path is UNC, a reserved
    device name in any component (`CON`, `PRN`, `AUX`, `NUL`, `COM0-9`, `LPT0-9`, `COM¹²³`, `CONIN$`,
    `CONOUT$`, `CLOCK$`, also with an extension or trailing spaces), a component ending in a dot or a
    space, an alternate data stream, and Ferrum's own files in the config dir
    (`hotlist.cache.json`, `hotlist-location.json`, `hotlist-approvals.json`).
  - A saved location that later fails the check (for example its folder was replaced by a junction)
    puts the hotlist in the `unsafe` state: read-only, the cached copy is shown, nothing is read from
    or written to that place, and command items do not run.
  - Nothing at the new place → **Move** copies the current hotlist there.
  - A hotlist already there → **Use the existing file**. A file is never overwritten silently.
- Writes go to a temp file `~hotlist.json.<pid>.tmp` (sync clients skip `~*.tmp`) that is flushed with
  `sync_all` before it is renamed into place, so a power cut cannot leave an empty hotlist (FER-467 R6).
  The previous on-disk version becomes `hotlist.json.bak`.
- Before each write the current file is opened **with read sharing only** (nobody else can write,
  delete or rename it while Ferrum holds it), hashed through that handle and compared with the version
  Ferrum read. If it changed, the write is refused (`conflict`), the file is reloaded and the user
  repeats the change. If it did not, that same handle renames it to `.bak`, and the temp file is moved
  into the free name without replacing anything: there is no window between the check and the write
  (FER-467 R6). If another program creates a new `hotlist.json` in the instant the name is free, theirs
  is kept, Ferrum reports a conflict and the verified previous version is in `.bak`. A file someone else
  has open for writing is reported as a conflict instead of being replaced.
- File missing at a custom location, unreadable (drive or sync not ready), empty/invalid, or a cloud
  placeholder → the last valid copy (`hotlist.cache.json`) is shown **read-only** and nothing is written
  until the file reads again. Cloud placeholders (offline / recall-on-access attributes) are never
  read, so they are never downloaded by Ferrum.
- Errors that reach the UI are codes only; paths are never logged.

## `command` items

- Launched with `ShellExecuteW "open"`, program and arguments separate (arguments quoted per
  `CommandLineToArgvW`), never through a shell string.
- The UI sends the item's **place in the tree** (index path) plus what it showed (title, command,
  arguments). The backend runs it only if the stored hotlist has exactly that item at exactly that
  place; otherwise it answers `stale`, the UI reloads and nothing starts (this also covers a hotlist
  that changed while the menu was open).
- FER-467 R4: an approval is bound to the item's full identity: the hotlist file in use, the index
  path, the titles of every submenu on the way and the item's own title (together they decide which
  accelerator keys reach it), the program, the arguments, and the working directory (items have none
  today; ShellExecute gets no directory, but the field is part of the key). The key is a SHA-256 over
  length-prefixed fields, stored in `hotlist-approvals.json` in the local config dir (never in the
  synced hotlist). Moving an approved command to another place or accelerator, renaming it or its
  submenu, changing its arguments, or switching to another hotlist file asks again. Approvals from
  before FER-467 are not reused (users confirm once more).
- FER-467 R5: the confirmation is a **native Windows message box** (`MessageBoxW`, owned by the Ferrum
  window, Cancel is the default button) whose text the backend builds from the stored item. Order
  (FER-467 H2): the warning for scripts and shortcuts first, then the program, then each argument
  on its own line exactly as the program receives it, then menu path and hotlist file, then the
  question. For display only, each argument (and the program, menu and file lines) is shortened to
  200 characters plus `… (N characters)`, and the whole text stays within about 2000 characters
  (arguments that do not fit are counted as "not shown"); what runs is always the full arguments. The
  webview cannot draw, answer or skip it: there is no "confirmed" parameter any more. After **OK** the
  backend checks again that the item is still the one shown before recording the approval.
- Commands never run on load, import or migration.
- A command is a local program path or a bare program name. URI schemes (`https:`, `ms-msdt:`,
  `search-ms:` …), anything starting with two separators in any mix (`\\`, `//`, `\/`, `/\`), the NT
  prefix `\??\`, the characters `" < > | * ?` and a `:` anywhere but after the drive letter (alternate
  data streams) are refused in the model, on import and in the editor. Shared vectors:
  `contract-fixtures/hotlist/command-cases.json` (Rust and TypeScript tests).
- For every target except a plain `.exe`/`.com` (so `.bat`/`.cmd`, shortcuts, dotted or extension-less
  names that PATHEXT may resolve to a batch file) arguments
  containing `" % & | < > ^` or a line break are refused (BatBadBut, CVE-2024-24576): model, import
  and run all check it.
- Text fields (title, path, target path, command, arguments, source) refuse Unicode format
  characters (bidi controls U+202A–202E / U+2066–2069 / U+200E-F / U+061C, zero-width, BOM, tags);
  ZWNJ/ZWJ stay allowed (so the native confirmation cannot be reordered either).
- The confirmation shows the exact command line the backend sends (each argument quoted by
  `quote_arg`; the same vectors are tested on both sides in
  `contract-fixtures/hotlist/quote-arg.json`), and warns for scripts (`.bat .cmd .ps1 .vbs .js
.hta .scr …`) and shortcuts (`.lnk .url`). Configure shows the same warning while editing.
- Launching runs on a blocking thread, never on the async workers.

## Opening files outside Ferrum (FER-467 R5)

`open_external`, **Associate With**, **Print** and `shell_open` hand ShellExecute only an existing
file or folder on a drive (`X:\...`) or on a network share (`\\host\share\...`), as accepted by
`path_guard::check_str`. The long-path spellings `\\?\X:\...` and `\\?\UNC\host\share\...` are
first turned into `X:\...` / `\\host\share\...` and then checked the same way; every other `\\?\`
or `\\.\` form (GLOBALROOT, volume GUIDs, devices, `\\.\UNC`) stays refused. URI schemes and protocol handlers (`https:`, `file:`, `ms-msdt:`,
`search-ms:`, `shell:`, `mailto:` …), bare or relative names (which Windows would look up in App Paths
or `PATH`), device and NT namespaces, the `pipe` / `mailslot` / `IPC$` shares, alternate data streams
and paths that do not exist are refused before anything is launched.

## Network folders (FER-467 R8)

A `dir` item (or its target path) on a network share, `\\host\share`, `//host/share`, WebDAV
`\\host@SSL\DavWWWRoot` or `\\?\UNC\host\share`, makes Windows connect to that host when it is
opened, and on SMB Windows offers the user's sign-in (NTLM) to it. A hotlist synced from elsewhere can
contain such items, so:

- the network check also recognises the device spellings that reach a host (FER-467 H1):
  `\\.\UNC\host`, `\??\UNC\host`, anything with `\Device\Mup\host` (also via `GLOBALROOT`),
  `\Device\LanmanRedirector\` and `\Device\WebDavRedirector\`, and any other `GLOBALROOT` path
  (host shown as `globalroot`). Such folders are refused in the hotlist anyway; this protects the
  history and any other place that shows them;
- they carry a `net` tag in the Ctrl+D menu, in the Command Palette's hotlist results and in the
  Alt+Down folder history (the history only lists folders already visited, so it only marks them);
- opening one from the hotlist (menu or palette) to a host that was never confirmed asks first, once
  per host ("Open a folder on another computer?"). The host of a folder a pane already shows counts as
  confirmed. Confirmed hosts are remembered per machine in this window's storage
  (`ferrum.network.knownHosts.v1`); without storage they are remembered for the session only;
- rendering the menu, the palette or the history never touches the network: the marks are string
  checks. A drive letter mapped to a share is not recognisable from the path and is not marked.

## Editing the tree (Configure)

Item kinds: **Item** = shortcut to a folder, **Submenu** = a group, **Separator** = a divider line,
**Command** = runs a program (it asks for confirmation first).

- **Drag** a row with the mouse (it starts after 4 px of movement; a plain click only selects). The
  middle third of a submenu row drops the item **into** it, as its last entry, also when the submenu
  is empty. The upper or lower part of a row drops it before or after that row. **Esc** cancels the
  drag. A submenu cannot be dropped into itself or its own contents, and the depth and item limits
  still apply ("That item can't be moved there.").
- **Into submenu** (`Ctrl+→`) moves the selected item to the end of the submenu directly above it;
  **Out of submenu** (`Ctrl+←`) moves it out, to the level of its submenu, right after it.
- **Up** / **Down** (`Ctrl+↑` / `Ctrl+↓`, also `Ctrl+PageUp` / `Ctrl+PageDown`) swap with the
  neighbour in the same list.
- **Add item / submenu / separator / command** inserts after the selected row; when the selected row
  is a submenu, the new entry goes **inside** it, at the end.
- Submenus show ▾ (open) or ▸ (folded); click the arrow, or press `←` / `→`, to fold or unfold.

Dragging uses pointer events, not the HTML5 drag-and-drop API, so it keeps working while dropping
files from Explorer onto the main window is enabled.

## Import from Total Commander

Configure › **Import from Total Commander…** reads `[DirMenu]` of a `wincmd.ini` (default
`%APPDATA%\GHISLER\wincmd.ini`, or a typed path), shows a preview with warnings, then merges into the
draft (add or replace); **OK** writes. The ini is only ever read.

Rules (verified on a real file; fixture: `contract-fixtures/hotlist/tc-dirmenu.ini` →
`tc-dirmenu.expected.json`, shared by the Rust and TypeScript tests):

| ini                    | result                                                       |
| ---------------------- | ------------------------------------------------------------ |
| `menuN=-Title`         | start of a submenu                                           |
| `menuN=--`             | **end of the submenu** (not a separator)                     |
| `menuN=-`              | separator                                                    |
| `cmdN=cd <path>`       | `dir` (drive letter upper-cased)                             |
| `cmdN=<other>`         | `command` (program split at its extension, rest = arguments) |
| `cmdN=cm_…` / `em_…`   | skipped with a warning (TC internal commands)                |
| `menuN` without `cmdN` | skipped with a warning                                       |
| `pathN`                | `targetPath`                                                 |
| numbering gap          | import stops there, with a warning (as Total Commander does) |

Titles stay verbatim, empty submenus are kept, UTF-16/UTF-8/ANSI inis decode, files over 8 MiB are
refused.
