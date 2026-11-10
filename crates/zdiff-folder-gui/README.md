# ZDiffFolder-GUI

![zdiff-folder-gui](img/showcase.png)

GUI for zdiff-folder crate

```
zdiff-folder-gui [<dir1> [<dir2>]] [-log]
```

No console window opens. `-log` writes the log to the terminal zdiff-folder-gui was started from, or to a new console window when there is none.

# Features
* Two-folder tree diff
    - Background scan and compare, rows show Pending until compared; counts and timings in the status line
    - Rebuilt rows (compare finished, sort, filters) keep the top row of the view in place, or its nearest shown folder
    - Breadcrumb path bars, right-click menu on rows (set as root, set as Folder 1/2, open in Explorer, copy path)
    - Tree guide lines, folder tint, dimmed equal rows
    - File type tag column per side (`img`, `bin`, `BP` for Blueprint `.uasset`, ...)
    - Sort by Name or State, cursor with keyboard navigation, next/previous differing file
    - Find with highlighted names
    - Blacklist and whitelist filters with presets
* Options bar
    - F: flat list by depth
    - D: diff only
    - P: show parent folders (with diff only)
    - L: likeness bar, how alike each pair is (lines, pixels or bytes)
    - T: tolerance, differing files that pass the size, modification time or likeness criteria count as Similar
    - Link horizontal scroll of both sides
* Roots at a git revision or a Perforce depot folder, changelist or shelf, picked from a "Source control" dropdown next to each path bar
* External diff tools
    - Tools window with presets (zdiff-gui, TortoiseMerge, WinMerge, Beyond Compare, p4merge, VS Code, ...), argument templates, rules and a Test button
    - Executables are found next to the app, at the build path or on PATH
    - Rows a tool can't diff are marked
* Duplicates tab
    - Size collisions hashed in the background, with progress and Cancel
    - Duplicate groups shown inline in the tree; Duplicates only, Flat, Show parent folders and Hash column toggles
    - Keep this, next/previous group, then Resolve recycles the non-keepers
* Customizable keybindings (Ctrl+O), style window

## Configuration

zdiff-folder-gui reads a `.env` file only from the folder of its executable (`target/<profile>/` for `cargo run`). The working directory and its parents are not searched. Real environment variables win over `.env` values. The log window shows which file was loaded, or that none was found.

# Todo
* Maybe fixed?? ____ Some PCs egui becomes white after first launch ____ 

## Duplicate file diff:
* Allow restoring of resolved files (maybe mark file names as red?)
