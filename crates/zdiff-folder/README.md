# zdiff-folder

Library crate for tree folder diffs

[zdiff-gui](../zdiff-gui/README.md#folder-diff) builds on this

# Features
* Folder tree model (`fs::FileSystemModel`) with a background scanner
* Pair compare: size then bytes, cached by metadata (least recently used pairs dropped past 256 MiB)
* Likeness measures (lines for text, pixels for images, bytes otherwise) for the likeness bar and tolerance
* Blacklist/whitelist filters
* Duplicate detection: size pre-grouping, then size + hash groups; resolve plans recycle the non-keepers
* Folder sides at a git revision (compared by blob id) or a Perforce depot folder, changelist or shelf (compared by digest)
* Blueprint detection for Unreal `.uasset` files
* External diff tool list with argument templates and rules

# Todo
* IDEA, catch files/folder with same name but at another depth
