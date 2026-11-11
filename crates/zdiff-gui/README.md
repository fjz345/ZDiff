# ZDiff-GUI

![zdiff-gui](img/showcase.png)

GUI for zdiff crate

# Features
* Myers diffing (Options > Myers Algo)
    - Linear version (default)
    - Linear MT version
    - Trace version
* Keybindings
    - Customizable (Ctrl+O), every toolbar toggle has an action; the middle, back and forward mouse buttons can be bound too
    - P4 integrated commands and quick diffs (F1-F4)
* Undo (Ctrl+Z, mouse back) and redo (Ctrl+Y, Ctrl+Shift+Z, mouse forward) of the latest action: a side's file or revision (path bar, Source control dropdown, browser, drop, quick diff, clear paths), swap sides and hunk reverts, in one history of up to 100 steps. Reverts made on the shown pair step first; going back to another pair drops their history (the files keep the reverted contents). Option toggles, viewer modes and scrolling are not undone. In the merge view the same keys undo and redo merge picks; a focused text field keeps its own Ctrl+Z
* Text diff view
    - Side by side, Unified, or a single side at full width with its changed lines tinted
    - Patch: the diff as the unified patch Export Diff writes (3 context lines, current ignore options), with a Copy patch button; find, goto line and hunk revert work only in the row views
    - Next/previous conflict (Ctrl+1/2, Alt+Up/Down), goto line (Ctrl+G)
    - Find (Ctrl+F) with whole word and regex
    - Toolbar `< X >` steppers (context rows, conflict, find match): drag the value sideways to pick one, double-click it to reset
    - Line pins: align chosen line pairs
    - Overview ruler beside the scrollbar; click or drag to scroll there
    - Collapsed context blocks expand, up to the enclosing scope's opening line
    - Word wrap with aligned rows, horizontal scrolling with optional linked sides
    - Syntax highlighting for ~190 languages (bat's syntax set), auto-detected per pane by file name, extension or first line, or picked from a filterable list (Options > Syntax Highlighting)
    - Diff status with -/+ counts and compute time per stage
    - Likeness bar at the right end of the toolbar (the same bar as zdiff-folder-gui's), with a dropdown of the metrics the viewer has (hover the bar for the formula): Text: matching lines, or tokens with the Token phase on, both honoring the ignore options (persisted); Hex: equal bytes at the same offset; Image: pixels within the tolerance under the image metric (the same setting as the image toolbar's Metric); Audio: 1 - RMS(left - right) / (RMS(left) + RMS(right)); Video: 1 - the timeline's mean frame difference / 255
    - Revert a hunk on disk, with undo/redo (refused after an external change); read-only Perforce files are offered a `p4 edit`
    - Occurrence highlight: selecting text in a row highlights its other occurrences in the visible rows, on both sides
    - Diff options (toolbar)
        + Ignore whitespace, comments, and regex patterns (with presets); ignored regex matches are dimmed
        + Ignore line endings (Alt+E): CRLF, LF and CR compare equal
        + Ignore file markers (Alt+M): a UTF-8 BOM at the start and the presence of a final line break
        + Highlight rows that differ: switches the diff marks on/off; its dropdown picks changed scopes, lines, tokens and/or characters
        + Diff phases (Options): Scope, Line and Token (both default), Character. Each runs only where the coarser one found a change; with none on, the lines just sit side by side
        + Inline ghost tokens
        + Diff only, with a context row count
        + Toggle all options (Alt+A)
    - In-memory results cache: each diff stage keeps its last 6 results and each side its last 6 file loads (256 MB in all), keyed by the file contents and the options that stage uses, so toggling an option back or returning to an earlier pair shows at once. Unchanged local files (same size and modification time) and fixed revisions (`#rev`, `@change`, full commit SHA) are not read again; head, branch and index sides always are. Refresh and Recompute Diff compute from scratch
* Sides
    - Breadcrumb path bar per side, and a folder browser over that side to pick another file
    - Local paths, Perforce depot paths, changelists and shelves, git revisions, picked from a "Source control" dropdown per side; its "Back to ..." entry returns to the side's previous revision of the same file
    - Diff a depot file against its local file (Ctrl+L), a shelved file against its base revision (Ctrl+B)
* Viewers, picked from the file kind (override per diff); mismatched types show each side in its own viewer
    - Hex: Bytes or Strings mode (no hunk revert in Strings), overview strip, next/previous difference, goto offset, value inspector (ints, floats, string, endian toggle)
    - Image: side by side, A/B, difference, heatmap, histogram, swipe and overlay; channel filter, tolerance, Nearest/Linear sampling, linked zoom and pan
    - Image metric (Exact, Neighborhood 3x3/5x5/7x7, SSIM) for the difference, heatmap and changed count, with the tolerance on its 0 to 255 scale; hover the count for the mean difference (mean SSIM). Alignment of differently sized images: top left, center or scale to fit, for drawing and comparing. Both are saved; the video difference timeline uses the same metric
    - Audio: waveform lanes, overlay, spectrograms with their dB difference, sample difference lanes, metadata table with loudness
    - Video: frame compare with timestamp matching, scrubber and frame steps, a difference timeline, Audio and Metadata tabs (the audio decodes when the Audio tab first opens and is kept until the pair changes)
    - Video seeking: dragging the scrubber or the timeline shows low-res previews (about 320 px wide, decoded for the whole video in the background) at once, and the exact frames land when the drag pauses or ends. The frames around the cursor are decoded ahead into a cache (256 MB per side), so frame steps and going back and forth show at once.
    - Zooming out stops at fit: the whole image (or 1:1 for an image smaller than the view), the whole audio length
    - Double-clicking an image or audio mode button resets that mode's own settings (A/B interval, swipe divider, overlay opacity, spectrogram window and ranges)
    - Audio and video need ffmpeg: `cargo xtask fetch-ffmpeg` (see [ffmpeg/README.md](../../ffmpeg/README.md))
    - PDB (MSF 7.00): diffed as a sorted text dump: header (version, machine, stripped, optionally GUID/age/timestamp), modules, public symbols, global symbols (procedures with signatures and sizes, data, typedefs, constants; no locals), and one line per class/struct/union/enum with its fields. The toolbar's PDB menu (persisted) has Public only (as stripped), so a full PDB compares clean against its stripped copy, Ignore addresses/RVAs and Ignore GUID/age/timestamp (on by default); a change reloads the dumps. Capped at 1,000,000 lines with a truncation marker. A PDB that doesn't parse is shown in Hex; Hex and Strings on a PDB show its real bytes. Dumps can't be reverted into or merged. zdiff-folder-gui still compares PDBs by bytes, so a rebuild always differs there
* 3-way merge view: picks, chunk editing, undo/redo, conflict navigation, save (Ctrl+S)
* Style window: text, syntax, hex and image colors, presets, and the look of empty and collapsed rows
* Frame rate counter at the right end of the menu bar, marked idle once frames stop; clicking it (or Options > Frame Time Breakdown) shows the average and max time per frame stage over the last 120 frames

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

A crash (panic) appends its message, source location and backtrace to `crash.log` next to the saved state (`%APPDATA%\ZDiff\data\crash.log` on Windows) before the process exits.

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
