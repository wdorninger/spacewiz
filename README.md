<p align="center">
  <img src="docs/logo.svg" width="112" height="112" alt="spacewiz logo">
</p>

<h1 align="center">spacewiz</h1>

<p align="center">
  Disk space analyzer for Windows with sunburst and treemap views.
</p>

<p align="center">
  <a href="https://github.com/wdorninger/spacewiz/releases/latest"><img src="https://img.shields.io/github/v/release/wdorninger/spacewiz?label=release&color=2f6fd6" alt="Latest release"></a>
  <a href="https://github.com/wdorninger/spacewiz/releases/latest"><img src="https://img.shields.io/github/downloads/wdorninger/spacewiz/total?label=downloads&color=2f6fd6" alt="Downloads"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20x64-2f6fd6" alt="Windows x64">
  <img src="https://img.shields.io/badge/built%20with-Rust-555" alt="Built with Rust">
</p>

<p align="center">
  <a href="https://github.com/wdorninger/spacewiz/releases/latest"><b>Download</b></a> ·
  <a href="docs/usage.md">Usage</a> ·
  <a href="docs/configuration.md">Configuration</a> ·
  <a href="docs/development.md">Development</a>
</p>

---

spacewiz scans a drive or folder and shows where the space goes. The folder in focus sits in the centre and its
contents are arranged in rings around it, one ring per directory level, each segment sized by its share. Hover
for details, click a folder to zoom in.

It is a single executable with no installer, and it runs without administrator rights.

## Features

- **Fast scanning**: reads the NTFS Master File Table directly when run as administrator, indexing a full drive
  in seconds. Without elevation, a parallel directory scan covers about 3.3 million entries in roughly 20 seconds.
- **Live results**: the chart fills in while the scan runs and can be explored before it finishes.
- **Sunburst and treemap** views, coloured by top-level folder, file type or age.
- **Search** with wildcards, multiple terms and size filters such as `*.iso >1GB`, highlighted in the chart.
- **Cleanup hints** for temp folders, caches, build output, crash dumps and the Recycle Bin. Nothing is deleted
  automatically.
- **Duplicate finder** using size, partial and full content hashes (xxh3). Online-only cloud files are skipped
  and hard links are not reported.
- **File operations**: open, show in Explorer, copy path, rescan, move to the Recycle Bin, drag and drop.
- **Light and dark theme**, following Windows by default.

## Screenshots

<p align="center">
  <img src="docs/sunburst.png" alt="Sunburst view of a drive" width="880">
</p>

<p align="center">
  <img src="docs/treeview.png" alt="Treemap view" width="880">
</p>

## Quick start

1. Download `spacewiz-<version>-windows-x64-portable.zip` from the
   [latest release](https://github.com/wdorninger/spacewiz/releases/latest).
2. Extract it to any folder and run `spacewiz.exe`.
3. Select a drive, choose **Scan folder…**, or drop a folder onto the window.

Requires Windows 10 or 11, 64-bit. For the fast MFT scan on an administrator account, use the **Fast scan**
button in the toolbar.

## Documentation

| Page | Contents |
|---|---|
| [Usage](docs/usage.md) | Scanners, views, keyboard and mouse, search syntax, cleanup, duplicates, command line, limitations |
| [Configuration](docs/configuration.md) | `config.toml` settings and where they are stored |
| [Development](docs/development.md) | Building from source, release packaging, project layout |
