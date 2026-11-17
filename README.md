# ZDiff

![zdiff-gui](crates/zdiff-gui/img/showcase.png)
![zdiff-folder-gui](crates/zdiff-folder-gui/img/showcase.png)

Started as a tool to find and delete duplicate files with different names. It has grown into a diff suite with a file diff and a folder diff app.

Good reference & aim for this project: https://www.scootersoftware.com/

# Features
* File diff ([zdiff-gui](./crates/zdiff-gui)): token-based diff of text with scope, line, token and character phases, plus hex, image, audio and video viewers and a 3-way merge view
* Folder diff ([zdiff-folder-gui](./crates/zdiff-folder-gui)): two-folder tree diff with background compare, likeness, tolerance and filters
* Duplicate finder: size + hash grouping, pick a keeper, recycle the rest
* Git and Perforce: diff files and folders at a git revision, or a Perforce revision, changelist or shelf
* Works as the diff and merge tool for p4, P4V, git, TortoiseSVN and TortoiseGit
* Shared keybinding editor, find, style window and presets in both apps

# Crates
| Crate | Purpose | Type |
| :--- | :--- | :--- |
| [**zcommon**](./crates/zcommon) | Shared utilities and egui widgets | Library |
| [**zdiff**](./crates/zdiff) | File-level diff | Library |
| [**zdiff-gui**](./crates/zdiff-gui) | egui app for zdiff | Application |
| [**zdiff-folder**](./crates/zdiff-folder) | Folder-level diff | Library |
| [**zdiff-folder-gui**](./crates/zdiff-folder-gui) | egui app for zdiff-folder | Application |
| [**xtask**](./xtask) | `cargo xtask fetch-ffmpeg` ([ffmpeg](./ffmpeg/README.md)) | Tool |

# Build
```
cargo build --workspace
cargo run -p zdiff-gui                          # file diff
cargo run -p zdiff-gui -- <folder> <folder>     # folder diff; --folder alone reopens the last folders
cargo run -p zdiff-folder-gui                   # folder diff alone, for development
```
`zdiff-gui.exe` is the one shipped executable: two existing folders or `--folder` open the folder diff, anything else the file diff. The folder diff opens a file diff in a new process of the `zdiff-gui.exe` next to the running exe, so the development `zdiff-folder-gui` needs `zdiff-gui` built too. The audio and video viewers need ffmpeg (`cargo xtask fetch-ffmpeg`).

# Todo

---

# Notes
Hobby project for personal use. Windows is the primary platform.
