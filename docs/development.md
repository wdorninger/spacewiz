# Development

[README](../README.md) · [Usage](usage.md) · [Configuration](configuration.md) · **Development**

## Building

Requires Rust 1.85 or later (edition 2024) and the MSVC toolchain.

```powershell
cargo build --release
cargo test
```

The executable is written to `target\release\spacewiz.exe`. The C runtime is linked statically, so no
redistributable is needed. Build from PowerShell or a Developer Command Prompt; Git Bash may pick up GNU `link`
instead of the MSVC linker.

## Release package

`package.ps1` builds the portable release zip in `dist\`. It replaces local build paths in the executable with
generic ones.

## Project layout

| Path | Contents |
|---|---|
| `src/scan/mft.rs` | NTFS Master File Table reader |
| `src/scan/walk.rs` | Parallel directory scan |
| `src/scan/mod.rs` | Scan selection, progress reporting, exclusions |
| `src/tree.rs` | In-memory file tree and aggregation |
| `src/sunburst.rs` | Sunburst layout, rendering and hit testing |
| `src/treemap.rs` | Treemap layout, rendering and hit testing |
| `src/chart.rs` | Colours, highlighting and tooltips shared by both views |
| `src/search.rs` | Search query parser and matcher |
| `src/cleanup.rs` | Cleanup rules |
| `src/dupes.rs` | Duplicate file detection |
| `src/ui.rs` | Main window |

The user interface is built with [egui](https://github.com/emilk/egui) and eframe.
