# Ferrum 0.2.8 — changelog

Changes since 0.2.7 (`docs/release/v0.2.7/`). Windows 10/11 x64 only.
Everything in the 0.2.7 [CHANGELOG](../v0.2.7/CHANGELOG.md) and earlier is still part of the product. This file lists only what is new or changed.

## Changed

- **Recent view order** (FER-599): the Recent view (Ctrl+E) now opens sorted by last use, newest first, with files and folders mixed (no folders-on-top grouping inside Recent). Clicking a column header still sorts within Recent. Leaving Recent restores the tab's own sort, which is never changed. The tab's Reverse order setting does not apply inside Recent.

## Internal, not enabled (Automation)

As in 0.2.5, the Automation UI is behind the build flag `VITE_FERRUM_AUTOMATION_UI`, which is off in release builds.

## Not included in this release

- Automation (see above), MSI installer, code signing, auto-update, Linux and macOS.
