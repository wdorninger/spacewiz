# Usage

[README](../README.md) · **Usage** · [Configuration](configuration.md) · [Development](development.md)

## Scanning

Select a drive in the left panel, use **Scan folder…**, or drop a folder onto the window. The chart is updated
while the scan runs and can be navigated before it finishes.

spacewiz uses one of two scanners:

| Scanner | When | Speed |
|---|---|---|
| Master File Table (MFT) | NTFS volumes, running as administrator | A full drive in seconds |
| Directory scan | Everything else | Parallel `FindFirstFileExW` on a thread pool; a system drive with about 3.3 million entries takes roughly 20 seconds with a warm cache |

On an administrator account the toolbar offers a **Fast scan** button that restarts the program elevated. Other
accounts always use the directory scan.

- Folders can be excluded by full path (for example `C:\Windows\WinSxS`) or by name (for example
  `node_modules`). See [Configuration](configuration.md).
- Junctions and symbolic links are not followed. Hard-linked files are counted once.
- In directory-scan mode, folders the current user may not list are skipped; the toolbar shows how many.

## Views

- **Sunburst**: the folder in focus is in the centre, its contents in rings around it, one ring per directory
  level. The angle of each segment is proportional to its size.
- **Treemap**: squarified rectangles, nested by folder.

Switch between them in the toolbar. Both views can be coloured by top-level folder, by file type, or by the age
of the newest file in each folder. The free space of the drive is shown as a separate segment.

Hovering an item shows its size, its share of the parent and of the current view, the number of files and
folders, the modification date and the full path.

## Keyboard and mouse

| Action | Input |
|---|---|
| Zoom into folder | Click, mouse wheel up |
| Up one level | Click the centre, mouse wheel down, `Backspace`, `Alt+↑` |
| Back / forward | `Alt+←` / `Alt+→`, mouse side buttons |
| Search | `Ctrl+F`; `Esc` clears the search and highlighting |
| Rescan current folder | `F5` |
| Open file | Double-click |
| Context menu | Right-click |

The breadcrumb bar above the chart jumps to any parent folder.

## Search

Matches are highlighted in the chart and listed separately.

| Example | Matches |
|---|---|
| `report` | Names containing "report" |
| `*.iso` | Names matching the wildcard (`*`, `?`) |
| `*.mp4 *.mkv` | Either term |
| `>500MB` | Items larger than 500 MB (also `<`, `>=`, `<=`) |
| `file:` | Files only (`folder:` for folders only) |

Terms can be combined, for example `*.iso >1GB`.

## Analysis

- **Cleanup hints** point to temporary folders, browser and package-manager caches, build output, crash dumps
  and the Recycle Bin. They are suggestions only; nothing is deleted automatically.
- **Duplicate files** are found by size, then a partial hash, then a full content hash (xxh3). Online-only cloud
  files (OneDrive, Synology Drive) are skipped so they are not downloaded, and hard links are not reported as
  duplicates.
- **Largest files** and **space per file type** are listed in their own panels.

## File operations

From the context menu: open, show in Explorer, copy path, rescan a folder, and move to the Recycle Bin (with
confirmation). Drag an item out of the window to copy it elsewhere.

## Command line

```text
spacewiz.exe [path]                   Open the window and scan the given folder
spacewiz.exe <path> --search <query>  Scan, then run a search
spacewiz.exe <path> --find-dupes      Scan, then look for duplicate files
spacewiz.exe --scan <path> [--walk]   Scan without a window and print totals and the largest items
spacewiz.exe --dupes <path>           List duplicate files without a window
spacewiz.exe --console                Show the log in a console window
```

## Limitations

- The MFT reader supports NTFS only. FAT32, exFAT, ReFS and network drives are always scanned directory by
  directory.
- In directory-scan mode, folders that Windows does not allow the current user to list (for example other
  users' profiles) are skipped.
- Alternate data streams are not included in sizes.
- The first scan after a restart is slower, because directory metadata is not yet in the file system cache.
