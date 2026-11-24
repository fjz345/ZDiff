# ZDiff

![zdiff-gui](crates/zdiff-gui/img/showcase.png)
![zdiff-gui folder diff](crates/zdiff-gui/img/showcase-folder.png)

Started as a tool to find and delete duplicate files with different names. It has grown into a diff suite with a file diff and a folder diff in one app.

Good reference & aim for this project: https://www.scootersoftware.com/

# Features
* File diff ([zdiff-gui](./crates/zdiff-gui)): token-based diff of text with scope, line, token and character phases, plus hex, image, audio and video viewers and a 3-way merge view
* Folder diff ([zdiff-gui](./crates/zdiff-gui/README.md#folder-diff)): two-folder tree diff with background compare, likeness, tolerance and filters
* Duplicate finder: size + hash grouping, pick a keeper, recycle the rest
* Git and Perforce: diff files and folders at a git revision, or a Perforce revision, changelist or shelf
* Works as the diff and merge tool for p4, P4V, git, TortoiseSVN and TortoiseGit
* One window for both: shared path bars, keybinding editor, style window and presets; a row of the folder diff opens as a file diff in place, Back returns

# Crates
| Crate | Purpose | Type |
| :--- | :--- | :--- |
| [**zcommon**](./crates/zcommon) | Shared utilities and egui widgets | Library |
| [**zdiff**](./crates/zdiff) | File-level diff | Library |
| [**zdiff-gui**](./crates/zdiff-gui) | egui app for zdiff and zdiff-folder | Application |
| [**zdiff-folder**](./crates/zdiff-folder) | Folder-level diff | Library |
| [**xtask**](./xtask) | `cargo xtask fetch-ffmpeg` ([ffmpeg](./ffmpeg/README.md)), `cargo dist` (release build, LICENSE and, on Windows, ffmpeg into `target/dist/ZDiff_<version>/`) | Tool |

# Build
```
cargo build --workspace
cargo run -p zdiff-gui                          # file diff
cargo run -p zdiff-gui -- <folder> <folder>     # folder diff
```
`zdiff-gui.exe` is the one executable: two folders (or a lone folder) show the folder diff, files the file diff, and the sides can be changed between the two in the window. `--folder` only marks depot paths as folders. The audio and video viewers need ffmpeg (`cargo xtask fetch-ffmpeg`).

Linux and macOS build the same way; the executable is `zdiff-gui`. On Linux it needs a Wayland or X11 session, and xdg-desktop-portal for the file dialogs. `cargo xtask fetch-ffmpeg` is Windows only: install ffmpeg with the package manager (`apt install ffmpeg`, `dnf install ffmpeg`, `brew install ffmpeg`) and zdiff-gui finds ffmpeg and ffprobe on PATH. macOS is untested.

# Todo

---

# Notes
Hobby project for personal use. Windows is the primary platform.
