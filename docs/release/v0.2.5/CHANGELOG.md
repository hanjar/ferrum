# Ferrum 0.2.5 — changelog

Changes since 0.2.4 (`docs/release/v0.2.4/`). Windows 10/11 x64 only. Entries refer to public pull requests (`#n` on `hanjar/ferrum`).
Everything in the 0.2.4 [CHANGELOG](../v0.2.4/CHANGELOG.md) and earlier is still part of the product. This file lists only what is new or changed.

## Features

- **Image viewer on F3** (#74): F3 on an image (including SVG) opens a full-window viewer. Zoom to Fit or 25 to 400 % with `+` / `-`, `0` (Fit), `1` (100 %) or Ctrl+wheel; drag to pan a zoomed image; Left/Right, PageUp/PageDown, Home/End step through the images in the pane; Esc, F3 or Enter closes it and leaves the cursor on the last image viewed. F3 on anything else, or on an image inside an archive, still opens Quick View.
- **SVG thumbnails** (#74): Thumbnail View (Ctrl+Shift+F2) now draws `.svg` files instead of showing an icon.
- **Thumbnail sizes** (#74): four levels (Small, Medium, Large, Extra large) per pane, from the Show menu, Larger/Smaller thumbnails in Ctrl+K, or Ctrl+wheel over the grid. View presets remember the size; presets saved before 0.2.5 load as Medium.

## Fixes and safety

- **Copy out of an archive subfolder** (#75): F5 from a folder inside an archive (for example `x.zip\arsip\export\`) now copies the selected items straight into the target folder. Before, Ferrum recreated the archive's folder path under the target (`target\arsip\export\...`). Copying from the archive root and unpacking with Alt+F9 are unchanged.
- **SVG is never loaded as a page** (#74): SVG thumbnails and the viewer show SVG only as an image (`<img>` with a `data:` URL), so scripts or external links inside an SVG never run or load.
- **Archive extraction checks the full member name first** (#75): the archive folder prefix is removed only after the whole member path has passed the usual path checks, and the copy is refused if any member falls outside the selected folder.

## Internal, not enabled (Automation)

As in 0.2.4, the Automation UI is behind the build flag `VITE_FERRUM_AUTOMATION_UI`, which is off in release builds.

## Not included in this release

- Automation (see above), MSI installer, code signing, auto-update, Linux and macOS.
