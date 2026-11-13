# Configuration

[README](../README.md) · [Usage](usage.md) · **Configuration** · [Development](development.md)

Settings are changed in the program and stored in `config.toml` next to the executable, together with the log
(`logs\spacewiz.log`). Set the environment variable `SPACEWIZ_HOME` to store both elsewhere.

| Setting | Description |
|---|---|
| `ui.view` | `sunburst` or `treemap` |
| `ui.color_by` | `folder`, `type` or `age` |
| `ui.size_mode` | `logical` (file length) or `allocated` (size on disk) |
| `ui.ring_depth` | Number of levels shown |
| `scan.exclude` | Folders to skip, by full path or by name |
| `scan.prefer_mft` | Use the MFT reader when running as administrator |
| `dupes.min_size_mb` | Smallest file size considered for duplicate detection |

The theme follows the Windows light or dark setting by default and can be changed in the program.
