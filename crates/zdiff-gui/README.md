# ZDiff-GUI

![zdiff-gui](img/showcase.png)

GUI for zdiff crate

# Features
* Myers diffing (Options > Myers Algo)
    - Linear version (default)
    - Linear MT version
    - Trace version
* Keybindings
    - Customizable (Ctrl+O), every toolbar toggle has an action
    - P4 integrated commands and quick diffs (F1-F4)
* Text diff view
    - Side by side, Unified, or a single side at full width with its changed lines tinted
    - Next/previous conflict (Ctrl+1/2, Alt+Up/Down), goto line (Ctrl+G)
    - Find (Ctrl+F) with whole word and regex
    - Line pins: align chosen line pairs
    - Overview ruler beside the scrollbar; click or drag to scroll there
    - Collapsed context blocks expand, up to the enclosing scope's opening line
    - Word wrap with aligned rows, horizontal scrolling with optional linked sides
    - Syntax highlighting for ~190 languages (bat's syntax set), auto-detected per pane by file name, extension or first line, or picked from a filterable list (Options > Syntax Highlighting)
    - Diff status with -/+ counts and compute time per stage
    - Revert a hunk on disk, with undo/redo (refused after an external change); read-only Perforce files are offered a `p4 edit`
    - Occurrence highlight: selecting text in a row highlights its other occurrences in the visible rows, on both sides
    - Diff options (toolbar)
        + Ignore whitespace, comments, and regex patterns (with presets); ignored regex matches are dimmed
        + Highlight rows that differ: switches the diff marks on/off; its dropdown picks changed scopes, lines, tokens and/or characters
        + Diff phases (Options): Scope, Line and Token (both default), Character. Each runs only where the coarser one found a change; with none on, the lines just sit side by side
        + Inline ghost tokens
        + Diff only, with a context row count
        + Toggle all options (Alt+A)
* Sides
    - Breadcrumb path bar per side, and a folder browser over that side to pick another file
    - Local paths, Perforce depot paths, changelists and shelves, git revisions, picked from a "Source control" dropdown per side
    - Diff a depot file against its local file (Ctrl+L), a shelved file against its base revision (Ctrl+B)
* Viewers, picked from the file kind (override per diff); mismatched types show each side in its own viewer
    - Hex: Bytes or Strings mode, overview strip, next/previous difference, goto offset, value inspector (ints, floats, string, endian toggle)
    - Image: side by side, A/B, difference, heatmap, histogram, swipe and overlay; channel filter, tolerance, Nearest/Linear sampling, linked zoom and pan
    - Audio: waveform lanes, overlay, spectrograms with their dB difference, sample difference lanes, metadata table with loudness
    - Video: frame compare with timestamp matching, scrubber and frame steps, a difference timeline, Audio and Metadata tabs
    - Zooming out stops at fit: the whole image (or 1:1 for an image smaller than the view), the whole audio length
    - Double-clicking an image or audio mode button resets that mode's own settings (A/B interval, swipe divider, overlay opacity, spectrogram window and ranges)
    - Audio and video need ffmpeg: `cargo xtask fetch-ffmpeg` (see [ffmpeg/README.md](../../ffmpeg/README.md))
* 3-way merge view: picks, chunk editing, undo/redo, conflict navigation, save (Ctrl+S)
* Style window: text, syntax, hex and image colors, presets, and the look of empty and collapsed rows

## Occurrence highlighting
Selecting text within one row (drag, double-click a word, triple-click the line) highlights every other occurrence of that exact text in the visible rows on both sides, in purple. Matching is case-sensitive. Selections shorter than 2 characters, whitespace-only selections and selections spanning rows highlight nothing. The highlight clears with the selection (Escape, or a click elsewhere).

egui keeps its label selection private, so the pane mirrors it for one row at a time through egui's own cursor logic (`ui_egui/occurrence.rs`). Changing the selection with Shift+arrow keys isn't followed. The highlight follows the selection one frame later.

Budget: 1 ms per frame for the search, else the trigger would have become double-click only. Measured with the release build (`cargo test --release`, the `[profile.release]` settings) on `generated/scattered_20k` (the zdiff bench fixture, 20,437 rows), searching both sides of every window of 60, 100 and 200 visible rows across the whole file, with the needles `value`, `e_`, ` =`, `va` and `compute_index(`:

| visible rows | median | worst window |
|---|---|---|
| 60 | 6-8 µs | 35 µs |
| 100 | 8-12 µs | 25 µs |
| 200 | 17-24 µs | 44 µs |

**Decision: live selection.** The worst window is under 5% of the budget.

A degenerate row such as 2000 times `a`, searched for `aa`, has a match at every character. Each row therefore highlights at most 128 matches (`MAX_OCCURRENCES_PER_ROW`). With the cap, 100 such rows cost 0.46 ms median and 0.64 ms worst; 200 rows cost 0.85 ms median and 1.1 ms worst. Without it, 100 rows cost 9 ms. Highlights past the cap in a row aren't drawn.

## Use as a diff and merge tool
zdiff-gui takes p4merge-style, git-style, GNU-style and Tortoise-style arguments (`zdiff-gui --help` lists them). Each launch opens its own window, and the process exits when you close it. The examples use `C:\Tools\zdiff-gui.exe`. Replace it with your path (for example `target\release\zdiff-gui.exe`).

No console window opens. Add `-log` anywhere in the arguments to write the log to the terminal zdiff-gui was started from, or to a new console window when there is none (from cmd or PowerShell the prompt returns right away and the log interleaves with it).

A merge launch opens the merge view with the left file as theirs and the right file as yours. The process exits 0 only when the output was saved with no unresolved conflicts and not changed since; otherwise 1, including when the launch arguments are invalid.

### p4
```
p4 set P4DIFF=C:\Tools\zdiff-gui.exe
p4 set P4MERGE=C:\Tools\zdiff-gui.exe
```
`p4 diff` passes two files (a diff). `p4 resolve` (`m`) passes `base theirs yours merged`. zdiff-gui reads four files as a merge.

### P4V
Edit > Preferences.
- Diff: Other application. Location `C:\Tools\zdiff-gui.exe`, Arguments:
  ```
  %1 %2
  ```
- Merge: Other application. Location `C:\Tools\zdiff-gui.exe`, Arguments:
  ```
  -nb Base -nl Theirs -nr Yours -nm Result %b %1 %2 %r
  ```
  P4V has no placeholders for file names, so the labels are fixed role names.

### git
In `~/.gitconfig` (inside the quotes, use forward slashes in the path):
```
[diff]
    tool = zdiff
[difftool "zdiff"]
    cmd = \"C:/Tools/zdiff-gui.exe\" \"$LOCAL\" \"$REMOTE\"
[merge]
    tool = zdiff
[mergetool "zdiff"]
    cmd = \"C:/Tools/zdiff-gui.exe\" \"$BASE\" \"$LOCAL\" \"$REMOTE\" \"$MERGED\"
    trustExitCode = true
```
`git difftool -d` passes folders, which zdiff-gui rejects (use zdiff-folder-gui).

### TortoiseSVN / TortoiseGit
Settings > External Programs (TortoiseSVN) or Settings > Diff Viewer / Merge Tool (TortoiseGit), choose External.
- Diff Viewer:
  ```
  C:\Tools\zdiff-gui.exe /base:%base /mine:%mine /basename:%bname /minename:%yname
  ```
  Base is shown on the left, mine on the right.
- Merge Tool:
  ```
  C:\Tools\zdiff-gui.exe /base:%base /theirs:%theirs /mine:%mine /merged:%merged /basename:%bname /theirsname:%tname /minename:%yname /mergedname:%mname
  ```
  Theirs is shown on the left, mine on the right.

## Configuration

zdiff-gui reads a `.env` file only from the folder of its executable (`target/<profile>/` for `cargo run`). The working directory and its parents are not searched. Real environment variables win over `.env` values. The log (`-log`) shows which file was loaded, or that none was found.

## Known bugs
* lost_focus not called correctly on paths when holding down mouse: https://github.com/emilk/egui/issues/2142

## Todo
* Batch DiffRow performance optimzation
* user defined comparison per file format
