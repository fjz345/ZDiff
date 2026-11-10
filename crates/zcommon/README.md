# ZCommon

Common library crate shared by both apps.

# Modules
* `hash`: blake3 hashing (mmap) and `HashService`, a background worker pool that hashes paths on request
* `pool`, `logger` (`LogCollector` captures logs for the in-app log window)
* `path`: path segments and common parent
* `universal_path`: local paths, Perforce depot paths (`//depot/...#rev`, changelists, shelves) and git revisions
* `vcs`: `git` and `p4` runners and parsers (streams, digests, shelves, print at a revision)
* `kinds`: file kind table (text, image, video, audio, model, archive, executable, debug symbols (`pdb`), Unreal asset/map, binary)
* `ffmpeg`: ffmpeg/ffprobe lookup, runner and probe
* `find`: find matcher (whole word, regex)
* `presets`: preset store for filters and regex ignore patterns
* `style`: style model and the shared style file
* `zcommon-ui` crate (separate): split table with per-side horizontal scroll, keymap and keymap editor, find popup, breadcrumb path bar, presets menu, style window, toggle button
