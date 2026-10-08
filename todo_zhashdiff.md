# zhashdiff / zhashdiff-gui todo

Input for `/write-a-prd`. PRDs named here are in `E:\Github\AI\ZHashDiff\prd\`. "Draft" means the PRD is written but not sliced into issues yet. `Now:` notes were checked against the code on 2026-10-06.

## Bugs

- **Scroll flicker at the bottom.** Scrolling down at the end of the tree flickers, as if it keeps trying to scroll past the end. The top is fine. Not investigated.
- **View resets every so often on large folders.**
  - Now (suspect, not verified): every 30 s the autosave logs `{:?}` of the whole app state on the UI thread (`crates/zhashdiff-gui/src/gui/ui_egui/app.rs:456`). That includes the file system views and the visible rows, which are skipped by serde but not by `Debug`. Debug logging is forced on in every build. cross-app-hygiene (Draft) removes the dump.
  - Reproduce first, to tell a stall from a real rebuild.

## Performance and responsiveness

- **Large folders are slow.** Suspected cause: no row virtualization.
  - Now: drawing is already virtualized (`body.rows`, `crates/zhashdiff-gui/src/gui/ui_egui/fs_tree.rs:812`). The per-frame O(rows) work is elsewhere: `two_folder_cursor_rows` is built at least twice per frame, plus once per key, and `drawn_rows` is allocated every frame (`fs_tree.rs:590-671`). Profile first.
- **Background load and compare with spinners.** Never stall the UI thread while scanning, comparing or hashing. Show spinners on rows whose result is still pending.
  - See: folder-compare-pipeline (Draft) covers background scan and compare, progressive per-row states, and cancel.
- **Counts and timing.** Show the number of diffs and how long the view took to build, like zdiff-gui's status text.
  - See: the cross-app-hygiene status line (Draft) and folder-compare-pipeline (Draft).

## Tree view

- **Folders vs files.** Folders look different from files (a border or a color). Rows whose state is equal are dimmed.
- **Tree lines.** Draw tree-graph guide lines in the row indentation.
- **File type tags.** A colored tag column by extension. Include the usual formats and many game-dev ones:
  - `img` for .png, .tga, ...;
  - `vid` for videos;
  - `txt` for human-readable text;
  - `bin` for anything unidentified (the default);
  - `BP` for Blueprint .uasset, `UASS` for other .uasset.
  - Q: Telling a Blueprint .uasset from another one means reading the asset header, not just the extension. Is that OK? Should the extension table be shared with zdiff-gui's viewer map (new-file-types issue 03)?
- **Likeness bar.** A similarity metric per row, drawn as a progress bar with `NN%` inside.
  - Q: Which metric: byte-level, zdiff's line diff ratio, or perceptual for images? Tied to the tolerance mode below.
- **Horizontal scrollbar.** A horizontal scrollbar that appears when needed, with zdiff-gui's split/linked behavior and a toggle in the diff options.
  - Now: zdiff-gui has per-side bars with a link toggle (`crates/zdiff-gui/src/gui/ui_egui/h_scroll.rs`).
- Center-column drag and the reusable table: see Shared.

## Diff options bar

- **An options bar like zdiff-gui's.** Square buttons with symbols. Flat already exists. Add:
  - diff only;
  - show parent folders;
  - a tolerance mode, where a file that's similar enough counts as equal, with its own settings.
  - Q: What should tolerance measure: the likeness metric, a size delta, or mtime?
- **The same bar in duplicates mode.** Implement the obvious toggles there too (see Duplicates mode).

## Filters

- **Separator syntax.** Allow space and other tokens as separators besides `,` and newline. A literal space is written `\ `.
  - Now: `PatternList::new` splits on `,` and `\n` and trims each entry (`crates/zhashdiff/src/filter.rs:51`), so inner spaces are part of the pattern.
  - Revises: folder-filter-sort story 9 (done). Saved filters that contain spaces would parse differently.
  - Q: Migrate saved filter text, or accept the break?
- **Built-in presets.** Common blacklist/whitelist presets: "no bin", "txt only", and so on.
- **User presets.** Save and load named blacklists and whitelists.
  - Same shape as the regex ignore presets in todo_zdiff.md.

## Navigation and entry points

- **Clickable path segments.** Each segment of a selected folder path can be clicked to go to that parent folder.
  - See: folder-app-shell (Draft) makes the path boxes editable.
  - Q: Does clicking a segment make that folder the side's new root?
- **Right-click menu on folders.** Set as the new root, "Set as Folder 1", "Set as Folder 2", and other actions where they fit.
  - See: folder-app-shell (Draft) puts copy, move, delete and open in Explorer out of scope.
- **Launch arguments.** Open folders from the command line.
  - See: folder-app-shell (Draft) adds `zhashdiff-gui <dir1> <dir2>`.
  - Q: Anything beyond that?

## External diff tool

- **The options window.** Rework the External Diff Tool window: its setup and its features.
  - Now: Options > Set external diff tool, with ZDiff and Tortoise defaults (`ui_egui/app.rs:250`). folder-app-shell (Draft) fixes how the default tool is found.
  - Q: What's missing: argument templates, a tool per extension, a test button?
- **"Can this be diffed?"** Ask the configured tool whether it can diff a file, with a setting that highlights diffable and non-diffable rows.
  - Q: Few tools expose this. When the tool is zdiff-gui, use its extension map (new-file-types)?

## Duplicates mode

- **Unify with the folder view.** Make duplicates mode work like the folder view (tree, cursor, options bar). Keep its own features: resolve, hash view, progress bar and async hashing.
  - Now: a separate pane with its own tree (`ui_egui/duplicate_files_pane.rs`).

## Settings and assets

- **Remove the Font Size option.** egui's Ctrl+- and Ctrl++ already handle it.
  - Now: egui's built-in keyboard zoom is active. Both apps only call `set_pixels_per_point` once at startup (`ui_egui/app.rs:162`).
  - See: folder-app-shell (Draft) removes the NYI Font size, Setup colors and Set Theme items. cross-app-hygiene (Draft) says "there is no zoom", which is stale. Its zoom work comes down to the DPI base, persistence and clamping.
- **App icon.** Give zhashdiff-gui an exe and window icon. Use zdiff-gui's for now.
  - Now: zhashdiff-gui's `build.rs` only sets `ZDIFF_BIN_PATH` and embeds no icon. zdiff-gui's icon pipeline is in `crates/zdiff-gui/build.rs` and its `main.rs:49`.

## Debug

- **Debug open entries.** Debug menu entries that open example folder pairs, like zdiff-gui's Load $A-$D.
  - Now: the Debug menu only has Clear Root Paths and Clear FileSystemModels (`ui_egui/app.rs:234`).
  - Candidates in `test/`: `test_similar_1`, `test_similar_2`, `test_similar_2_one_deeper`, `1/`.

## Unscoped

- General visual improvements.
- General UX improvements.

## Shared with zdiff-gui

Described in the Shared section of todo_zdiff.md:

- Reusable keybindings (zdiff-gui's system, used by both apps).
- Reusable find (searching names and paths here).
- Reusable table and columns, plus dragging the center column.
- UI style options (colors and visuals).
- Common-parent path hint on the two folder paths.
- Logging quality and spam.
- VCS branch picking (git and Perforce, plus an optional folder sub-path).
