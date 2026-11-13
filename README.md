<p align="center">
  <img src="docs/logo.svg" width="96" alt="spacewiz">
</p>

<h1 align="center">spacewiz</h1>

<p align="center">
  Disk space analyzer for Windows with a sunburst chart and a treemap view.<br>
  <a href="https://github.com/wdorninger/spacewiz/releases/latest">Download</a> ·
  <a href="#usage">Usage</a> ·
  <a href="#building-from-source">Build</a>
</p>

<p align="center">
  <img src="docs/sunburst.png" alt="Sunburst view of a drive" width="880">
</p>

spacewiz scans a drive or folder and shows how the space is distributed. The folder in focus is drawn in the
centre; its contents are arranged in rings around it, one ring per directory level, with each segment's angle
proportional to its size. Hovering a segment shows its details, clicking a folder zooms into it.

It ships as a single executable and does not need an installer or administrator rights.

## Features

**Scanning**

- On NTFS volumes, when run as administrator, the Master File Table is read directly. A full drive is
  indexed in seconds.
- Without administrator rights, directories are enumerated in parallel (`FindFirstFileExW` on a thread
  pool). A system drive with about 3.3 million entries takes roughly 20 seconds with a warm cache.
- The chart is updated while a folder scan is running and can be navigated before the scan finishes.
- Folders can be excluded by full path (e.g. `C:\Windows\WinSxS`) or by name (e.g. `node_modules`).
- Junctions and symbolic links are not followed; hard-linked files are counted once.

**Visualisation**

- Sunburst chart and squarified treemap, switchable from the toolbar.
- Colouring by top-level folder, by file type, or by age of the newest file in each folder.
- Hover details: size, share of parent and of the current view, file and folder count, modification
  date, full path.
- Animated zoom; navigation by click, mouse wheel, breadcrumbs, back/forward history and keyboard.
- Free space of the drive is shown as a separate segment.
- Light and dark theme, following the Windows setting by default.

**Analysis**

- Search with wildcards, multiple terms and size filters (`*.iso >1GB`); matches are highlighted in the
  chart and listed separately.
- Cleanup hints for temporary folders, browser and package manager caches, build output, crash dumps and
  the Recycle Bin. These are suggestions only; nothing is deleted automatically.
- Duplicate file detection by size, partial hash and full content hash (xxh3). Online-only cloud files
  (OneDrive, Synology Drive) are skipped so that they are not downloaded; hard links are not reported as
  duplicates.
- Lists of the largest files and of space used per file type.

**File operations**

- Open, show in Explorer, copy path, rescan a folder, move to the Recycle Bin (with confirmation).
- Drop a folder onto the window to scan it; drag an item out of the window to copy it elsewhere.

<p align="center">
  <img src="docs/treeview.png" alt="Treemap view" width="880">
</p>

## Installation

Download `spacewiz-<version>-windows-x64-portable.zip` from the
[releases page](https://github.com/wdorninger/spacewiz/releases), extract it to any folder and run
`spacewiz.exe`.

Requirements: Windows 10 or 11, 64-bit.

## Usage

Select a drive in the left panel, use **Scan folder…**, or drop a folder onto the window.

The fast MFT scan requires administrator rights. On an administrator account the toolbar offers a
**Fast scan** button that restarts the program elevated; other accounts always use the directory scan.

### Command line

```text
spacewiz.exe [path]                   Open the window and scan the given folder
spacewiz.exe <path> --search <query>  Scan, then run a search
spacewiz.exe <path> --find-dupes      Scan, then look for duplicate files
spacewiz.exe --scan <path> [--walk]   Scan without a window and print totals and the largest items
spacewiz.exe --dupes <path>           List duplicate files without a window
spacewiz.exe --console                Show the log in a console window
```

### Keyboard and mouse

| Action                   | Input                                                    |
|--------------------------|----------------------------------------------------------|
| Zoom into folder         | Click, mouse wheel up                                    |
| Up one level             | Click the centre, mouse wheel down, `Backspace`, `Alt+↑` |
| Back / forward           | `Alt+←` / `Alt+→`, mouse side buttons                    |
| Search                   | `Ctrl+F`; `Esc` clears the search and highlighting       |
| Rescan current folder    | `F5`                                                     |
| Open file                | Double-click                                             |
| Context menu             | Right-click                                              |

### Search syntax

| Example        | Matches                                         |
|----------------|-------------------------------------------------|
| `report`       | Names containing "report"                       |
| `*.iso`        | Names matching the wildcard (`*`, `?`)          |
| `*.mp4 *.mkv`  | Either term                                     |
| `>500MB`       | Items larger than 500 MB (also `<`, `>=`, `<=`) |
| `file:`        | Files only (`folder:` for folders only)         |

### Configuration

Settings are changed in the program and stored in `config.toml` next to the executable, together with the
log (`logs\spacewiz.log`). Set the environment variable `SPACEWIZ_HOME` to store them elsewhere.

| Setting               | Description                                           |
|-----------------------|-------------------------------------------------------|
| `ui.view`             | `sunburst` or `treemap`                               |
| `ui.color_by`         | `folder`, `type` or `age`                             |
| `ui.size_mode`        | `logical` (file length) or `allocated` (size on disk) |
| `ui.ring_depth`       | Number of levels shown                                |
| `scan.exclude`        | Folders to skip, by path or name                      |
| `scan.prefer_mft`     | Use the MFT reader when running as administrator      |
| `dupes.min_size_mb`   | Smallest file size considered for duplicates          |

## Building from source

Requires Rust 1.85 or later (edition 2024) and the MSVC toolchain.

```powershell
cargo build --release
cargo test
```

The executable is written to `target\release\spacewiz.exe`. The C runtime is linked statically, so no
redistributable is needed. Build from PowerShell or a Developer Command Prompt; Git Bash may pick up GNU
`link` instead of the MSVC linker.

### Project layout

| Path                 | Contents                                                |
|----------------------|---------------------------------------------------------|
| `src/scan/mft.rs`    | NTFS Master File Table reader                           |
| `src/scan/walk.rs`   | Parallel directory scan                                 |
| `src/scan/mod.rs`    | Scan selection, progress reporting, exclusions          |
| `src/tree.rs`        | In-memory file tree and aggregation                     |
| `src/sunburst.rs`    | Sunburst layout, rendering and hit testing              |
| `src/treemap.rs`     | Treemap layout, rendering and hit testing               |
| `src/chart.rs`       | Colours, highlighting and tooltips shared by both views |
| `src/search.rs`      | Search query parser and matcher                         |
| `src/cleanup.rs`     | Cleanup rules                                           |
| `src/dupes.rs`       | Duplicate file detection                                |
| `src/ui.rs`          | Main window                                             |

The user interface is built with [egui](https://github.com/emilk/egui) / eframe.

## Limitations

- The MFT reader supports NTFS only; FAT32, exFAT, ReFS and network drives are always scanned directory by
  directory.
- In directory-scan mode, folders that Windows does not allow the current user to list (for example other
  users' profiles) are skipped. The number of skipped folders is shown in the toolbar.
- Alternate data streams are not included in sizes.
- The first scan after a restart is slower, because directory metadata is not yet in the file system cache.
