# zdiff / zdiff-gui todo

Input for `/write-a-prd`. PRDs named here are in `E:\Github\AI\ZHashDiff\prd\`. "Draft" means the PRD is written but not sliced into issues yet. `Now:` notes were checked against the code on 2026-10-06.

## Bugs

- **Yellow row highlight at conflict 0.** The row highlight stays on after the conflict counter goes back to 0, which means no conflict is selected.
  - Now (likely cause, not reproduced): `get_scroll_to_row` rewrites `active_highlights` only when there is a new scroll target (`crates/zdiff-gui/src/gui/diff_ctx.rs:912`). Stepping from 1 to 0 gives no target, so the last conflict, goto or find highlight stays.
  - Q: At 0, clear every highlight, or only the conflict one?
- **Pivot line doesn't work.** It used to work. The pivot should pin left line L to right line R. It's for log files where both sides are known to reach the same state at some line.
  - Now (the regression's cause is not investigated): the pivot only shifts one side's finished rows by a row offset (`align_rows_to_pivot`, `diff_ctx.rs:1232`). It doesn't re-anchor the diff. `need_invalidation` fires only when both lines are > 0 and changed (`crates/zdiff/src/diff_builder.rs:75`), so clearing the pivot never recomputes.
  - See: file-view-state-input (Draft), which makes the pivot transient and clearable and keeps it across recomputes. Also the open question in diff-row-fidelity (Draft) on pivot vs ghost rows.
  - Q: Should the diff be re-anchored at the pivot (diff before and after it separately, with the pivot lines forced equal) instead of shifting rows? Re-anchoring is what makes log diffs line up.

## Performance

- **Option toggles take 1.8-3.6 ms (release).** Toggling a trivial option such as Highlight Rows takes that long, and other toggles are similar.
  - Now: `highlight_rows` and `ghost_rows` are in `DiffBuilderOptions::need_invalidation` (`diff_builder.rs:75`), so each toggle rebuilds every row. H only changes a background color, so it could be render-only.
  - Q: Is the target "render-only options never rebuild", or a time budget per toggle?

## Diff options and toolbar

- **Toolbar symbols.** Replace the letter labels with symbols:
  - whitespace: a visible-space glyph (`_` or `·`);
  - comments: `//` instead of `C`;
  - highlight rows: a horizontal bar.
  - Now: W, C, regex, H, ghost, link, wrap, D (`crates/zdiff-gui/src/gui/ui_egui/diff_pane.rs:268`).
- **Toggle-all key.** One key that toggles every diff option: whitespace, comments, ghost rows, linked scroll, the diff-only context count, and so on.
  - Q: Does it mean all on/all off, or swap with the previous set (snapshot and restore)?
- **Regex ignore presets.** Preset buttons for common noise (timestamps such as `[time]`, GUIDs, hex addresses), plus user-saved patterns.
  - Now: regex ignore is free text only (`ignore_patterns_btn`, diff-engine issue 07).
  - Same shape as the filter presets in todo_zhashdiff.md.
- **Selectable diff phases.** A setting for which levels Myers runs at: scope, line, token/character.
  - Now: line then token (`myers_diff_path`). The scope phase is designed but not built (design note in `crates/zdiff/README.md`).
- **Debug view of the phases.** A debug mode that shows each phase's result: scope pairs, line hunks, and the token diff inside a line. Same PRD as the item above.

## Layout and visuals

- **Middle column.** Move the revert buttons to the left and right of the diff-state symbol, and color the symbol.
  - Now: `Column::exact(12.0)` holding "≠" (`diff_pane.rs:598`).
- **+/- colors.** Green and red for the add/delete counts, and more color in general.
  - See: diff-row-fidelity (Draft) changes the counts to changed lines.
- **Language selector icons.** Icons in Options > Code Language: the Rust crab, the C++ logo, and so on.
  - Q: Bundle logo images (each with its own license), or use drawn glyphs or text badges?
- **Transparent app icon.** Replace the white background of `crates/zdiff-gui/img/icon.png` with transparency.
  - Now: 256x256 ARGB with opaque white corners. `build.rs` embeds it in the exe, and `main.rs:49` uses it as the window icon.
- Movable center column, UI style options, common-parent path hint: see Shared.

## Input and navigation

- **Horizontal scroll modifier.** Let the user choose which modifier turns the wheel into horizontal scroll: Shift or Ctrl.
  - Now: egui's `InputOptions::horizontal_scroll_modifier` (default Shift) does this. Ctrl is egui's `zoom_modifier`: Ctrl+wheel becomes `zoom_delta`, which the image viewer uses to zoom (`viewer/image.rs:553`).
  - Q: Ctrl only in the text viewer, reading raw wheel events there? Changing egui's zoom modifier globally breaks image zoom.
- **Default keybindings.** Ship useful defaults, for example Swap Source/Target.
  - Now: swap is only a File menu item (`crates/zdiff-gui/src/gui/app.rs:427`) and isn't a `Keybindings` field, so it needs an action first.
  - Q: Which actions and keys? Candidates: swap, each diff option, word wrap, link scroll, diff-only.
- **Menus close on click.** A plain button in the menu bar (File, Options, Debug) closes the menu. Toggling a radio or checkbox, or editing text, keeps it open.
  - Now: the menu bar uses `PopupCloseBehavior::CloseOnClickOutside` (`app.rs:398`), so no click inside closes it. egui 0.33 has `Ui::close()` for the plain buttons.
  - Assumed: "nav buttons" means the top menu bar.
- **Folder browser per side.** Clicking a side's parent folder path replaces that side's diff view with a file browser. Picking a file reopens the diff with that file on that side.
  - See: file-view-state-input (Draft) adds path boxes and recent pairs. No PRD covers browsing.

## Viewers

- **Mismatched types: show each file, not Hex.** A .png vs .txt pair shows the image on one side and the text on the other, with no diff. Hex stays available as a manual override.
  - Now: sides that resolve to different viewers fall back to Hex (`crates/zdiff-gui/src/gui/viewer.rs:214`).
  - Revises: new-file-types story 13 (done).
  - Q: What shows on a side that's binary with no viewer of its own: single-side Hex?
- **Image sampling mode.** An option for texture sampling, defaulting to nearest.
  - Now: magnification is already Nearest, minification is Linear with mipmaps (`viewer/image.rs:642`), and the difference mask is Nearest. Missing: the toggle, and Nearest when zoomed out.
- **More visualization modes per viewer.**
  - Now: image has side-by-side, difference, swipe and overlay. Text and hex have one each.
  - Q: Which modes? Needs examples before it can become a PRD.
- **Audio and video diff.** Viewers for audio and video with several views, for example a spectrogram (Fourier).
  - Q: Which formats? Is a decode dependency acceptable (ffmpeg, symphonia)? This is large and needs its own PRD.

## Integration and entry points

- **Command line for third-party use.** Better argument handling so zdiff-gui can replace p4merge and similar tools.
  - Now: only two positional paths (`app.rs:224`). revert-and-p4 issues 05 and 10 handle p4 temp paths. Display labels are out of scope in both file-view-state-input and zdiff-cli.
  - Q: Which conventions? p4 `%1 %2`, git difftool `$LOCAL $REMOTE`, p4merge-style labels (`-nl`/`-nr`), a merge output path.
- **3-way merge.** Resolve merge conflicts from base, theirs and yours into an output file. Needs its own PRD and builds on the command-line item.
- Git support and better Perforce support: see Shared (VCS branch picking).

## Debug

- **Debug open entries per viewer.** The Debug menu gets at least one quick-open entry per viewer type.
  - Now: Load $A-$D, all text (`app.rs:589`, `debug_assertions` only).
  - Candidates in `test/`: a PNG pair (image), `very_large_file.vhdx` (hex), `test_log/` (log, pivot), png vs txt (mismatch).

## Unscoped

- General visual improvements.
- General UX improvements.

## Shared (both apps)

Both todo files list these. One PRD per item covers both apps. todo_zhashdiff.md only points here.

- **Reusable table and columns.** One table/column API for both apps' side-by-side tables.
  - Now: both apps call egui_extras `TableBuilder` directly (`diff_pane.rs:488`, `crates/zhashdiff-gui/src/gui/ui_egui/fs_tree.rs:699`).
- **Drag the center column.** If possible, make the center column movable by dragging it. Today only its right-hand border moves the split, and the right side is locked.
  - Q: Drag the strip itself to move the split, or make both borders draggable?
  - Now: zdiff-gui's center column is `Column::exact(12.0)`. zhashdiff-gui's is `Column::auto().auto_size_this_frame(true)`, re-sized every frame (`fs_tree.rs:714`).
  - Do this with the table refactor.
- **Reusable keybindings.** zdiff-gui's customizable keybindings become a shared system that both apps use.
  - Now: `crates/zdiff-gui/src/gui/keybindings.rs` is one struct of named fields plus p4 quick-diffs. zhashdiff-gui hardcodes its keys (`cursor_keys`, `diff_jump_keys` in `fs_tree.rs:1288`).
- **Reusable find.** zdiff-gui's find becomes a shared system. In the folder app it searches names and paths.
  - See: file-view-state-input (Draft) improves find, but lists folder-app find as out of scope. It predates commits d29a7f1 and 8b48966 (find text highlight) and c151c5e (Tab/Esc), so refresh it before slicing.
- **UI style options.** An options window for colors and visuals:
  - syntax colors and the background;
  - how empty (Void) and collapsed rows look, including an optional angled-stripe pattern.
  - Now: out of scope in cross-app-hygiene and diff-row-fidelity. zhashdiff-gui's "Setup colors" and "Set Theme" are NYI stubs, which folder-app-shell removes. Needs its own PRD.
- **Common-parent path hint.** Subtly mark the part of both paths up to their lowest common parent folder.
  - Q: Mark the shared prefix or the differing tail?
- **Logging.** Better log quality and less spam.
  - See: cross-app-hygiene (Draft) sets per-build levels, caps the log, drops the 30 s state dump, and adds the status line.
  - Q: Which spam remains after that?
- **VCS branch picking.** Git support, and Perforce beyond what's there: pick branches or streams (plus an optional sub-path) to diff files or folders.
  - Now: Perforce has connection profiles, quick diffs and depot-to-local (revert-and-p4, done). No git. `UniversalPath` only knows local paths and `//depot/...#rev`, so a git revision needs a new variant.
  - Q: Shell out to `git` like `p4.exe`, or take a dependency (gitoxide, git2)? Which Perforce gaps remain?
