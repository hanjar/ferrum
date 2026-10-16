# Ferrum 0.2.0 — keyboard shortcuts

Windows only. Source of truth: the action registry `src/lib/actions.ts`
(`actions` array, base `main` @ `ac97ccd`). Compared with the RC1 table, the registry gained two
actions: `ai.addToChat` (`Ctrl+Shift+L`) and `ai.askAgent` (no shortcut); no existing shortcut
changed. Uniqueness of ids and shortcuts is enforced by `src/lib/actions.test.ts`
("registry ids and assigned shortcuts resolve uniquely"). No registry action carries a
`disabledReason`, so every listed action has a handler; some are only enabled in context (for
example a selection, or a cursor on a file).

"Ctrl" is Ctrl on Windows (the registry says `ctrlOrCmd`; macOS is untested).
The bottom key bar shows F2, F5, F6, F7, F8 (Shift+click on Delete runs
"Delete permanently") and the hint "Ctrl+K Commands".

## Keys handled by the pane, not by the registry

From `src/lib/components/FilePane.svelte` (`onListKey`):

| Key                       | Effect                                                                                |
| ------------------------- | ------------------------------------------------------------------------------------- |
| `Up` / `Down`             | Move the cursor; selects that single row                                              |
| `Shift+Up` / `Shift+Down` | Extend the selection range from the anchor                                            |
| `Ctrl+Up` / `Ctrl+Down`   | Move the cursor without changing the selection                                        |
| `Home` / `End`            | First / last visible row                                                              |
| `Ctrl+A`                  | Select all visible rows                                                               |
| A letter or digit         | Type-to-select: jump to the next name starting with it (repeat the same key to cycle) |

## Actions with a keyboard shortcut

### Navigate

| Shortcut        | Action                                                                                                                                                                                                                                                                                                                                                                                                                                | Registry id       | Menu               |
| --------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------- | ------------------ |
| `Tab`           | Switch to other pane (Tab)                                                                                                                                                                                                                                                                                                                                                                                                            | `pane.switch`     | Commands           |
| `Backspace`     | Go to parent folder                                                                                                                                                                                                                                                                                                                                                                                                                   | `nav.up`          | Commands           |
| `Ctrl+\`        | Go to root of the active pane's drive or UNC share (also from inside an archive); inactive in text fields.                                                                                                                                                                                                                                                                                                                            | `nav.root`        | Commands           |
| `Alt+D`         | Edit path (address bar with subfolder suggestions; Windows convention)                                                                                                                                                                                                                                                                                                                                                                | `nav.editPath`    | Navigate (palette) |
| `Alt+ArrowDown` | Folder history dropdown (Total Commander style): top = most visited folders with visit count, below the line = this tab's history, current folder last. `Up`/`Down` select, `Enter` or click navigates, `Esc` or `Alt+ArrowDown` closes. Not active while a text field has focus (in the address bar `Alt+ArrowDown` keeps its text-field behaviour). Frequent folders are hidden when activity recording is off or not yet confirmed | `nav.historyMenu` | Commands           |
| `Ctrl+F`        | Search in current folder                                                                                                                                                                                                                                                                                                                                                                                                              | `search.open`     | (palette only)     |
| `Escape`        | Close search                                                                                                                                                                                                                                                                                                                                                                                                                          | `search.close`    | (palette only)     |

**Tab between panes.** In the main window `Tab` and `Shift+Tab` only move focus between the file lists of the left and right pane (the list's cursor row shows the focus; first row if there is no cursor yet). From the tab strip, toolbar, sidebar or menu bar `Tab` jumps to the other pane's list from the active pane; with nothing focused it focuses the active pane's list. In a pane's text fields (filter, address bar without a suggestion to accept, view and `…` menus) `Tab` returns to the same pane's list. Dialogs, Settings, the command palette, popups, context menus, the AI chat and the search panel keep normal `Tab` navigation, and a single-pane layout does too.

Because `Tab` no longer visits buttons, reach those with the mouse or shortcuts: `Alt+D` address bar, `Ctrl+Tab` next tab, `Ctrl+K` command palette (every command, including filter and view), `Alt+Left` back, `Alt+Down` folder history.

### Tabs

| Shortcut | Action    | Registry id | Menu  |
| -------- | --------- | ----------- | ----- |
| `Ctrl+T` | New tab   | `tab.new`   | Show  |
| `Ctrl+W` | Close tab | `tab.close` | Files |

### File

| Shortcut       | Action                                                  | Registry id             | Menu                                  |
| -------------- | ------------------------------------------------------- | ----------------------- | ------------------------------------- |
| `Enter`        | Open with associated application / Open selected folder | `file.open`             | Files                                 |
| `F2`           | Rename                                                  | `file.rename`           | Files                                 |
| `F5`           | Copy to other pane                                      | `file.copy`             | Files                                 |
| `F6`           | Move to other pane                                      | `file.move`             | Files                                 |
| `F7`           | New folder                                              | `file.mkdir`            | Files                                 |
| `F8`           | Delete (Recycle Bin)                                    | `file.delete`           | Files                                 |
| `Shift+F8`     | Delete permanently                                      | `file.delete.permanent` | Files                                 |
| `Ctrl+M`       | Multi-Rename Tool                                       | `file.multiRename`      | Files                                 |
| `Alt+F5`       | Pack to archive (ZIP)                                   | `archive.pack`          | (palette only)                        |
| `Alt+F9`       | Unpack to other pane                                    | `archive.unpack`        | (palette only)                        |
| `Alt+Shift+F9` | Test archive(s)                                         | `archive.test`          | (palette only)                        |
| `Alt+Enter`    | Properties                                              | `file.properties`       | Show, Files > Attributes & properties |
| `Ctrl+L`       | Calculate Occupied Space                                | `file.occupiedSpace`    | Show, Files > Attributes & properties |
| `Ctrl+Shift+L` | Add to chat (attach the selection to the AI chat)       | `ai.addToChat`          | File context menu, palette            |

### Commands

| Shortcut          | Action                                                                | Registry id                | Menu     |
| ----------------- | --------------------------------------------------------------------- | -------------------------- | -------- |
| `Ctrl+D`          | Directory Hotlist                                                     | `commands.hotlist`         | Commands |
| `Alt+F10`         | CD Tree                                                               | `commands.cdTree`          | Commands |
| `Alt+ArrowLeft`   | Go Back                                                               | `commands.back`            | Commands |
| `Ctrl+1`          | Copy folder path (trailing `\`, e.g. `C:\dir\`)                       | `commands.copyFolderPath`  | Commands |
| `Ctrl+2`          | Copy file name(s) of selection, else cursor item (CRLF between items) | `commands.copyItemNames`   | Commands |
| `Ctrl+3`          | Copy full path(s) of selection, else cursor item (CRLF between items) | `commands.copyItemPaths`   | Commands |
| `Ctrl+U`          | Source↔Target                                                         | `commands.sourceTarget`    | Commands |
| `Ctrl+ArrowRight` | Open in Right Pane                                                    | `commands.openInRightPane` | Commands |
| `Ctrl+ArrowLeft`  | Open in Left Pane                                                     | `commands.openInLeftPane`  | Commands |
| `Ctrl+B`          | Branch View (With Subdirs)                                            | `commands.branch`          | Commands |
| `Alt+B`           | Background Transfer Manager                                           | `commands.btm`             | Commands |
| `Ctrl+K`          | Command palette                                                       | `palette.open`             | Commands |
| `Ctrl+,`          | Settings                                                              | `app.settings`             | Commands |
| `Alt+F7`          | Search                                                                | `commands.search`          | Commands |
| `Alt+Shift+D`     | Exit Disk Usage (back to Files) / Disk Usage                          | `commands.diskUsage`       | Commands |

### Show

| Shortcut        | Action            | Registry id          | Menu |
| --------------- | ----------------- | -------------------- | ---- |
| `Ctrl+R`        | Reread Source     | `show.refresh`       | View |
| `Ctrl+F1`       | Brief             | `show.brief`         | View |
| `Ctrl+F2`       | Full              | `show.full`          | View |
| `Ctrl+F8`       | Tree              | `show.tree`          | View |
| `Ctrl+Shift+F2` | Thumbnail View    | `show.thumbnails`    | View |
| `Ctrl+F3`       | Sort by Name      | `show.sortName`      | Sort |
| `Ctrl+F4`       | Sort by Extension | `show.sortExtension` | Sort |
| `Ctrl+F5`       | Sort by Time      | `show.sortTime`      | Sort |
| `Ctrl+F6`       | Sort by Size      | `show.sortSize`      | Sort |
| `Ctrl+F7`       | Unsorted          | `show.unsorted`      | Sort |
| `Ctrl+Q`        | Files             | `show.quickView`     | View |

### Select

| Shortcut  | Action       | Registry id   | Menu           |
| --------- | ------------ | ------------- | -------------- |
| `Shift+-` | Deselect all | `select.none` | (palette only) |

### View

| Shortcut       | Action                                | Registry id   | Menu |
| -------------- | ------------------------------------- | ------------- | ---- |
| `Ctrl+H`       | Hide hidden files / Show hidden files | `view.hidden` | View |
| `Ctrl+Shift+A` | Hide AI assistant / Show AI assistant | `view.aiChat` | Show |

### Favorites

| Shortcut       | Action                                                            | Registry id     | Menu           |
| -------------- | ----------------------------------------------------------------- | --------------- | -------------- |
| `Ctrl+Shift+D` | Unpin folder aktif dari Favorites / Pin folder aktif ke Favorites | `favorites.pin` | (palette only) |

### Mark

| Shortcut                    | Action                         | Registry id               | Menu |
| --------------------------- | ------------------------------ | ------------------------- | ---- |
| `Shift+F2`                  | Compare Directories            | `mark.compareDirectories` | Mark |
| `* (with or without Shift)` | Invert Selection               | `mark.invert`             | Mark |
| `+ (with or without Shift)` | Select All With Same Extension | `mark.sameExtension`      | Mark |

## Actions without a shortcut (menu or Ctrl+K palette)

| Action                                                                 | Registry id               | Menu                                  |
| ---------------------------------------------------------------------- | ------------------------- | ------------------------------------- |
| Associate With…                                                        | `file.associate`          | Show, Files > Attributes & properties |
| Edit Comment                                                           | `file.comment`            | Show, Files > Attributes & properties |
| Print                                                                  | `file.print`              | Files                                 |
| Encode File                                                            | `file.encode`             | Files > Split & encode                |
| Decode File                                                            | `file.decode`             | Files > Split & encode                |
| Quit                                                                   | `file.quit`               | Files                                 |
| Pack to archive (7z)                                                   | `archive.pack7z`          | (palette only)                        |
| Pack                                                                   | `file.pack`               | Files > Archive                       |
| Unpack Specific Files                                                  | `file.unpack`             | Files > Archive                       |
| Test Archive(s)                                                        | `file.testArchive`        | Files > Archive                       |
| Change Attributes                                                      | `file.attributes`         | Show, Files > Attributes & properties |
| Volume Label                                                           | `commands.volumeLabel`    | Commands                              |
| System Information                                                     | `commands.systemInfo`     | Commands                              |
| Open Terminal Here                                                     | `commands.prompt`         | Commands                              |
| Open Desktop Folder                                                    | `commands.desktop`        | Commands                              |
| Target = Source                                                        | `commands.targetSource`   | Commands                              |
| Create Checksum File(s)                                                | `file.createChecksum`     | Files > Checksums                     |
| Verify Checksums                                                       | `file.verifyChecksum`     | Files > Checksums                     |
| Checksum of Archive Member                                             | `file.archiveChecksum`    | Files > Checksums                     |
| Split File                                                             | `file.split`              | Files > Split & encode                |
| Combine Files                                                          | `file.combine`            | Files > Split & encode                |
| AI assistant settings…                                                 | `view.aiSettings`         | (palette only)                        |
| Ask AI agent about selection (read-only, needs an installed agent CLI) | `ai.askAgent`             | File context menu, palette            |
| Operations queue                                                       | `queue.panel`             | (palette only)                        |
| dark / Switch to light theme / Switch to dark theme                    | `theme.toggle`            | (palette only)                        |
| Use dark theme                                                         | `theme.dark`              | (palette only)                        |
| Use light theme                                                        | `theme.light`             | (palette only)                        |
| Follow system theme                                                    | `theme.system`            | (palette only)                        |
| Buka halaman Activity                                                  | `activity.open`           | (palette only)                        |
| Compare By Content                                                     | `file.compareContent`     | Files                                 |
| Synchronize Dirs                                                       | `commands.synchronize`    | Commands                              |
| Select Group                                                           | `mark.selectGroup`        | Mark                                  |
| Unselect Group                                                         | `mark.unselectGroup`      | Mark                                  |
| Select All                                                             | `mark.selectAll`          | Mark                                  |
| Unselect All                                                           | `mark.unselectAll`        | Mark                                  |
| Save Selection                                                         | `mark.save`               | Mark                                  |
| Restore Selection                                                      | `mark.restore`            | Mark                                  |
| Save Selection To File                                                 | `mark.saveFile`           | Mark                                  |
| Load Selection From File                                               | `mark.loadFile`           | Mark                                  |
| Copy Selected Names To Clipboard                                       | `mark.copyNames`          | Mark                                  |
| Copy Names With Path To Clipboard                                      | `mark.copyPaths`          | Mark                                  |
| Copy To Clipboard With All Details                                     | `mark.copyDetails`        | Mark                                  |
| Copy To Clipboard With Path+Details                                    | `mark.copyPathDetails`    | Mark                                  |
| Mark Newer, Hide Same Files                                            | `mark.newerFiles`         | Mark                                  |
| Comments (hide) / Comments                                             | `show.comments`           | View                                  |
| Custom View Modes                                                      | `show.customModes`        | View                                  |
| Hide Separate Tree / Separate Tree                                     | `show.separateTree`       | View                                  |
| Custom Columns                                                         | `show.customColumns`      | View                                  |
| Vertical Arrangement                                                   | `show.vertical`           | View                                  |
| All Files                                                              | `show.allFiles`           | Show                                  |
| Programs                                                               | `show.programs`           | Show                                  |
| Custom Filter                                                          | `show.filter`             | Show                                  |
| selected / Show All Files (leave Only Selected) / Only Selected Files  | `show.selectedOnly`       | Show                                  |
| Normal Order / Reversed Order                                          | `show.reverse`            | Sort                                  |
| Search in Separate Window                                              | `commands.searchSeparate` | Commands                              |

## Fallbacks for unsupported archive cases

These are limits of 0.2.0 (see `FEATURES.md` and `KNOWN-ISSUES.md`). Each row
gives the supported way around the limit using the shortcuts above.

| Wanted                                                                        | Status in 0.2.0                                                                                      | Fallback                                                                             |
| ----------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| Create a RAR archive                                                          | Only through your own WinRAR 7.00+ (`Rar.exe`, set in Settings); Ferrum has no RAR writer of its own | Without WinRAR: `Alt+F5` Pack to ZIP, or `Ctrl+K` then "Pack to archive (7z)"        |
| Add to, update or delete inside an existing archive                           | Unsupported (archives open read-only)                                                                | Unpack with `Alt+F9`, change the files, then pack again with `Alt+F5`                |
| Search inside an archive                                                      | Unsupported: `Ctrl+F` / `Alt+F7` match archive file names but never open them                        | Unpack with `Alt+F9` into the other pane, then search that folder                    |
| Compare By Content or checksum of a member of a RAR archive                   | Unsupported (RAR provider loads a whole member into memory, no bounded stream)                       | Unpack with `Alt+F9`, then compare or checksum the extracted file                    |
| Compare Directories or Synchronize across an archive                          | Unsupported: they work on disk trees only                                                            | Unpack with `Alt+F9`, then run `Shift+F2` on the folders                             |
| Quick View (`Ctrl+Q`) of a member of an encrypted archive                     | Refused (no password channel for preview)                                                            | Unpack with `Alt+F9` (the password prompt is supported), then Quick View the file    |
| Preview a PDF larger than 50 MiB, or a PDF inside an archive                  | Not previewed                                                                                        | `Enter` opens it with the associated Windows application; unpack first for archives  |
| Open `.tar`, `.gz`, `.tgz`, `.bz2`, `.xz`, `.jar`, `.apk`, `.cbz` as a folder | Unsupported (no provider)                                                                            | `Enter` hands the file to its Windows association                                    |
| Multi-volume RAR                                                              | Refused with an "unsupported" error (`archive_rar.rs`, "Multi-volume RAR not supported")             | Extract with the external tool that made it; no multi-volume 7z claim is made either |
