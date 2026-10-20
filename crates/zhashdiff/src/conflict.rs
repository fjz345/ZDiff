use std::{
    collections::{BTreeMap, HashMap},
    fmt::Display,
    fs,
    hash::Hash,
    io,
    path::{Path, PathBuf},
};

use crate::comparison::{DiskReader, FileMeta, PairSide, PairState, compare_pair};

/// Groups files by content hash. Only groups of two or more files are returned,
/// ordered by hash with each group's paths sorted, so the result does not depend
/// on the input order.
pub fn group_duplicates(
    files: impl IntoIterator<Item = (PathBuf, String)>,
) -> Vec<(String, Vec<PathBuf>)> {
    let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for (path, hash) in files {
        groups.entry(hash).or_default().push(path);
    }

    groups
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(hash, mut paths)| {
            paths.sort();
            (hash, paths)
        })
        .collect()
}

#[derive(Debug, PartialEq)]
pub struct Deletion {
    pub path: PathBuf,
    pub keeper: PathBuf,
}

/// What a resolve removes.
#[derive(Debug, Default, PartialEq)]
pub struct ResolutionPlan {
    /// Every non-keeper of each group whose keeper is one of its files, ordered by
    /// group hash, then by the group's order.
    pub deletions: Vec<Deletion>,
    /// Groups whose keeper is not one of their files. Nothing of them is planned.
    pub rejected: Vec<String>,
}

/// Groups without a keeper are left out.
pub fn plan_resolution(
    conflict_map: &HashMap<String, Vec<PathBuf>>,
    keepers: &HashMap<String, PathBuf>,
) -> ResolutionPlan {
    let mut hashes: Vec<&String> = conflict_map.keys().collect();
    hashes.sort();

    let mut plan = ResolutionPlan::default();
    for hash in hashes {
        let Some(keeper) = keepers.get(hash) else {
            continue;
        };
        let paths = &conflict_map[hash];
        // A keeper outside the group would otherwise remove every copy.
        if !paths.contains(keeper) {
            plan.rejected.push(hash.clone());
            continue;
        }
        plan.deletions.extend(
            paths
                .iter()
                .filter(|path| *path != keeper)
                .map(|path| Deletion {
                    path: path.clone(),
                    keeper: keeper.clone(),
                }),
        );
    }
    plan
}

#[derive(Debug, Default, PartialEq)]
pub struct ResolutionSummary {
    pub removed: Vec<PathBuf>,
    /// Files `delete` failed on, with its error. They are left in place.
    pub failed: Vec<(PathBuf, String)>,
}

/// Removes the planned files through `delete`, which is `recycle` outside of tests. A file is
/// only removed while its keeper exists, is another file and still has the same content.
pub fn execute_resolution<E: Display>(
    plan: &ResolutionPlan,
    mut delete: impl FnMut(&Path) -> Result<(), E>,
) -> ResolutionSummary {
    for hash in &plan.rejected {
        log::error!("Skipping conflict {hash}: its keeper is not one of its files");
    }

    let mut summary = ResolutionSummary::default();
    for Deletion { path, keeper } in &plan.deletions {
        if let Err(e) = verify_duplicate(path, keeper) {
            log::error!("Not removing {path:?}: {e}");
            summary.failed.push((path.clone(), e));
            continue;
        }
        match delete(path) {
            Ok(()) => {
                log::info!("Removed duplicate {path:?}");
                summary.removed.push(path.clone());
            }
            Err(e) => {
                log::error!("Failed to remove {path:?}: {e}");
                summary.failed.push((path.clone(), e.to_string()));
            }
        }
    }

    log::info!(
        "Resolution complete: {} removed, {} failed",
        summary.removed.len(),
        summary.failed.len()
    );
    summary
}

/// Hashes can be stale, and a junction or symlink makes one file appear under two paths.
fn verify_duplicate(path: &Path, keeper: &Path) -> Result<(), String> {
    let side = |path: &Path| -> Result<(PathBuf, FileMeta), String> {
        let error = |e: io::Error| format!("{path:?}: {e}");
        let real = path.canonicalize().map_err(error)?;
        let meta = fs::metadata(&real).map_err(error)?;
        let modified = meta.modified().map_err(error)?;
        let size = meta.len();
        Ok((real, FileMeta { size, modified }))
    };
    let (keeper_side, path_side) = (side(keeper)?, side(path)?);
    if keeper_side.0 == path_side.0 {
        return Err(format!("it is the keeper {keeper:?} under another path"));
    }
    fn pair_side((path, meta): &(PathBuf, FileMeta)) -> PairSide<'_> {
        PairSide {
            rel_path: "",
            path,
            meta: *meta,
            hash: None,
        }
    }
    let compare = compare_pair(
        &DiskReader,
        &pair_side(&keeper_side),
        &pair_side(&path_side),
    );
    match (compare.state, compare.error) {
        (PairState::Same, _) => Ok(()),
        (PairState::Different, Some(e)) => Err(format!("comparing with {keeper:?}: {e}")),
        (PairState::Different, None) => Err(format!("its content differs from {keeper:?}")),
    }
}

/// Moves `path` to the Recycle Bin.
pub fn recycle(path: &Path) -> Result<(), trash::Error> {
    trash::delete(path)
}

/// Whether recycling `path` can be undone. Unknown counts as no.
pub fn recycle_is_undoable(_path: &Path) -> bool {
    false
}

pub struct SizedFile {
    pub path: PathBuf,
    pub size: u64,
    pub hidden: bool,
}

/// Visible files sharing their size with another visible file, in input order.
pub fn size_collisions(files: &[SizedFile]) -> Vec<PathBuf> {
    let visible = || files.iter().filter(|file| !file.hidden);
    let mut counts: HashMap<u64, usize> = HashMap::new();
    for file in visible() {
        *counts.entry(file.size).or_default() += 1;
    }
    visible()
        .filter(|file| counts[&file.size] > 1)
        .map(|file| file.path.clone())
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct DuplicateGroup {
    pub size: u64,
    pub hash: String,
    pub paths: Vec<PathBuf>,
}

/// A group's id is its index: groups keep the input (tree) order of their first file.
pub fn group_by_size_and_hash<H: Eq + Hash + Display>(
    files: impl IntoIterator<Item = (PathBuf, u64, H)>,
) -> Vec<DuplicateGroup> {
    let mut groups: Vec<DuplicateGroup> = Vec::new();
    let mut group_of: HashMap<(u64, H), usize> = HashMap::new();
    for (path, size, hash) in files {
        let index = *group_of
            .entry((size, hash))
            .or_insert_with_key(|(size, hash)| {
                groups.push(DuplicateGroup {
                    size: *size,
                    hash: hash.to_string(),
                    paths: Vec::new(),
                });
                groups.len() - 1
            });
        groups[index].paths.push(path);
    }
    groups.retain(|group| group.paths.len() > 1);
    groups
}

/// Refused for a file in no group.
pub fn keep_this(
    groups: &[DuplicateGroup],
    keepers: &mut HashMap<String, PathBuf>,
    path: &Path,
) -> bool {
    let Some(group) = groups
        .iter()
        .find(|group| group.paths.iter().any(|p| p == path))
    else {
        log::error!("Not keeping {path:?}: it is not a file of any duplicate group");
        return false;
    };
    keepers.insert(group.hash.clone(), path.to_path_buf());
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io;
    use tempfile::{TempDir, tempdir};

    fn pair(path: &str, hash: &str) -> (PathBuf, String) {
        (PathBuf::from(path), hash.to_string())
    }

    fn group(hash: &str, paths: &[&str]) -> (String, Vec<PathBuf>) {
        (hash.to_string(), paths.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn group_duplicates_of_empty_input_is_empty() {
        assert!(group_duplicates(Vec::new()).is_empty());
    }

    #[test]
    fn group_duplicates_drops_singletons() {
        let files = vec![
            pair("a/one.txt", "h1"),
            pair("b/two.txt", "h2"),
            pair("c/three.txt", "h1"),
            pair("d/four.txt", "h3"),
        ];

        assert_eq!(
            group_duplicates(files),
            vec![group("h1", &["a/one.txt", "c/three.txt"])]
        );
    }

    #[test]
    fn group_duplicates_order_is_stable_across_input_permutations() {
        let files = vec![
            pair("x/1", "hb"),
            pair("x/2", "ha"),
            pair("y/1", "hb"),
            pair("y/2", "ha"),
            pair("z/1", "hb"),
            pair("z/2", "hc"),
        ];
        let expected = vec![
            group("ha", &["x/2", "y/2"]),
            group("hb", &["x/1", "y/1", "z/1"]),
        ];

        for rotation in 0..files.len() {
            let mut rotated = files.clone();
            rotated.rotate_left(rotation);
            assert_eq!(group_duplicates(rotated.clone()), expected);

            rotated.reverse();
            assert_eq!(group_duplicates(rotated), expected);
        }
    }

    #[test]
    fn group_duplicates_many_groups() {
        // Group g has g + 2 members; every tenth hash also gets a singleton that must be dropped.
        let mut files = Vec::new();
        for g in 0..100 {
            for member in 0..g + 2 {
                files.push((
                    PathBuf::from(format!("dir{member}/file{g:03}")),
                    format!("hash{g:03}"),
                ));
            }
            if g % 10 == 0 {
                files.push((PathBuf::from(format!("lonely{g}")), format!("single{g:03}")));
            }
        }

        let groups = group_duplicates(files);

        assert_eq!(groups.len(), 100);
        for (g, (hash, paths)) in groups.iter().enumerate() {
            assert_eq!(hash, &format!("hash{g:03}"));
            assert_eq!(paths.len(), g + 2);
            assert!(paths.is_sorted(), "paths of {hash} not sorted: {paths:?}");
        }
    }

    fn conflict_map(groups: &[(&str, &[&str])]) -> HashMap<String, Vec<PathBuf>> {
        groups
            .iter()
            .map(|(hash, paths)| (hash.to_string(), paths.iter().map(PathBuf::from).collect()))
            .collect()
    }

    fn keepers(keepers: &[(&str, &str)]) -> HashMap<String, PathBuf> {
        keepers
            .iter()
            .map(|(hash, path)| (hash.to_string(), PathBuf::from(path)))
            .collect()
    }

    fn paths(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn deleted(plan: &ResolutionPlan) -> Vec<PathBuf> {
        plan.deletions.iter().map(|d| d.path.clone()).collect()
    }

    #[test]
    fn plan_never_includes_the_keeper() {
        let groups = conflict_map(&[("h", &["a", "b", "c"])]);

        for keeper in ["a", "b", "c"] {
            let plan = plan_resolution(&groups, &keepers(&[("h", keeper)]));

            let others: Vec<&str> = ["a", "b", "c"]
                .into_iter()
                .filter(|path| *path != keeper)
                .collect();
            assert_eq!(deleted(&plan), paths(&others));
            assert!(plan.rejected.is_empty());
        }
    }

    #[test]
    fn plan_leaves_groups_without_a_keeper_untouched() {
        let groups = conflict_map(&[("h1", &["a", "b"]), ("h2", &["c", "d"])]);

        assert_eq!(
            plan_resolution(&groups, &HashMap::new()),
            ResolutionPlan::default()
        );
        assert_eq!(
            deleted(&plan_resolution(&groups, &keepers(&[("h2", "d")]))),
            paths(&["c"])
        );
        // A keeper for a group that no longer exists plans nothing and rejects nothing.
        assert_eq!(
            plan_resolution(&groups, &keepers(&[("gone", "a")])),
            ResolutionPlan::default()
        );
    }

    #[test]
    fn plan_covers_multiple_groups_in_hash_order() {
        let groups = conflict_map(&[
            ("hb", &["x/1", "y/1", "z/1"]),
            ("hc", &["q", "r"]),
            ("ha", &["x/2", "y/2"]),
        ]);

        let plan = plan_resolution(&groups, &keepers(&[("hb", "y/1"), ("ha", "x/2")]));

        assert_eq!(deleted(&plan), paths(&["y/2", "x/1", "z/1"]));
        assert!(plan.rejected.is_empty());
    }

    #[test]
    fn plan_rejects_a_keeper_that_is_not_a_member() {
        let groups = conflict_map(&[
            ("h1", &["a", "b"]),
            ("h2", &["c", "d"]),
            ("h3", &["e", "f"]),
        ]);

        // An empty path was the old conflicts table placeholder.
        let plan = plan_resolution(
            &groups,
            &keepers(&[("h1", ""), ("h2", "c"), ("h3", "elsewhere")]),
        );

        assert_eq!(deleted(&plan), paths(&["d"]));
        assert_eq!(plan.rejected, vec!["h1".to_string(), "h3".to_string()]);
    }

    /// a.txt, b.txt and c.txt in a temp dir, all with the same content.
    fn same_files() -> (TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempdir().unwrap();
        let [a, b, c] = ["a.txt", "b.txt", "c.txt"].map(|name| dir.path().join(name));
        for path in [&a, &b, &c] {
            fs::write(path, "same").unwrap();
        }
        (dir, a, b, c)
    }

    #[test]
    fn execute_resolution_removes_non_keepers_only() {
        let (_dir, a, b, c) = same_files();
        let groups = HashMap::from([("h".to_string(), vec![a.clone(), b.clone(), c.clone()])]);
        let plan = plan_resolution(&groups, &HashMap::from([("h".to_string(), a.clone())]));

        let summary = execute_resolution(&plan, |path| fs::remove_file(path));

        assert_eq!(summary.removed, vec![b.clone(), c.clone()]);
        assert!(summary.failed.is_empty());
        assert!(a.exists());
        assert!(!b.exists());
        assert!(!c.exists());
    }

    #[test]
    fn execute_resolution_of_a_placeholder_keeper_removes_nothing() {
        let (_dir, a, b, c) = same_files();
        let groups = HashMap::from([("h".to_string(), vec![a.clone(), b.clone(), c.clone()])]);
        let plan = plan_resolution(&groups, &HashMap::from([("h".to_string(), PathBuf::new())]));

        let summary = execute_resolution(&plan, |path| fs::remove_file(path));

        assert_eq!(summary, ResolutionSummary::default());
        assert!(a.exists());
        assert!(b.exists());
        assert!(c.exists());
    }

    #[test]
    fn execute_resolution_reports_a_failed_removal_and_leaves_the_file() {
        let (_dir, a, b, c) = same_files();
        let groups = HashMap::from([("h".to_string(), vec![a.clone(), b.clone(), c.clone()])]);
        let plan = plan_resolution(&groups, &HashMap::from([("h".to_string(), a.clone())]));

        let summary = execute_resolution(&plan, |path| {
            if path == b {
                Err(io::Error::other("file is in use"))
            } else {
                fs::remove_file(path)
            }
        });

        assert_eq!(summary.removed, vec![c.clone()]);
        assert_eq!(
            summary.failed,
            vec![(b.clone(), "file is in use".to_string())]
        );
        assert!(a.exists(), "the keeper is never touched");
        assert!(b.exists(), "a failed removal leaves the file in place");
        assert!(!c.exists());
    }

    fn plan_keeping(keeper: &Path, files: &[&Path]) -> ResolutionPlan {
        let groups = HashMap::from([(
            "h".to_string(),
            files.iter().map(|path| path.to_path_buf()).collect(),
        )]);
        plan_resolution(
            &groups,
            &HashMap::from([("h".to_string(), keeper.to_path_buf())]),
        )
    }

    fn failed_paths(summary: &ResolutionSummary) -> Vec<PathBuf> {
        summary
            .failed
            .iter()
            .map(|(path, _)| path.clone())
            .collect()
    }

    #[test]
    fn execute_resolution_removes_nothing_when_the_keeper_is_gone() {
        let (_dir, a, b, c) = same_files();
        let plan = plan_keeping(&a, &[&a, &b, &c]);
        fs::remove_file(&a).unwrap();

        let summary = execute_resolution(&plan, |path| fs::remove_file(path));

        assert!(summary.removed.is_empty());
        assert_eq!(failed_paths(&summary), vec![b.clone(), c.clone()]);
        assert!(b.exists());
        assert!(c.exists());
    }

    #[test]
    fn execute_resolution_keeps_a_file_that_changed_after_hashing() {
        let (_dir, a, b, c) = same_files();
        let plan = plan_keeping(&a, &[&a, &b, &c]);
        fs::write(&b, "diff").unwrap();

        let summary = execute_resolution(&plan, |path| fs::remove_file(path));

        assert_eq!(summary.removed, vec![c.clone()]);
        assert_eq!(failed_paths(&summary), vec![b.clone()]);
        assert!(b.exists());
    }

    #[cfg(windows)]
    #[test]
    fn execute_resolution_never_removes_the_keeper_through_a_junction() {
        let dir = tempdir().unwrap();
        let (real, link) = (dir.path().join("real"), dir.path().join("link"));
        fs::create_dir(&real).unwrap();
        fs::write(real.join("f.txt"), "same").unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .output()
            .unwrap()
            .status;
        assert!(status.success());
        let (file, alias) = (real.join("f.txt"), link.join("f.txt"));

        for (keeper, other) in [(&file, &alias), (&alias, &file)] {
            let plan = plan_keeping(keeper, &[&file, &alias]);

            let summary = execute_resolution(&plan, |path| fs::remove_file(path));

            assert!(summary.removed.is_empty());
            assert_eq!(failed_paths(&summary), vec![other.clone()]);
            assert!(file.exists());
        }
    }

    fn sized(path: &str, size: u64) -> SizedFile {
        SizedFile {
            path: PathBuf::from(path),
            size,
            hidden: false,
        }
    }

    fn hidden(path: &str, size: u64) -> SizedFile {
        SizedFile {
            hidden: true,
            ..sized(path, size)
        }
    }

    #[test]
    fn size_collisions_skips_unique_sizes() {
        let files = [sized("a", 1), sized("b", 2), sized("c", 3)];

        assert!(size_collisions(&files).is_empty());
    }

    #[test]
    fn size_collisions_queues_files_sharing_a_size_in_input_order() {
        let files = [
            sized("a", 10),
            sized("b", 20),
            sized("c", 10),
            sized("d", 30),
            sized("e", 20),
            sized("f", 10),
        ];

        assert_eq!(size_collisions(&files), paths(&["a", "b", "c", "e", "f"]));
    }

    #[test]
    fn size_collisions_excludes_hidden_files() {
        let files = [
            sized("a", 10),
            hidden("b", 10),
            sized("c", 20),
            sized("d", 20),
            hidden("e", 20),
        ];

        assert_eq!(size_collisions(&files), paths(&["c", "d"]));
    }

    fn hashed(path: &str, size: u64, hash: &str) -> (PathBuf, u64, String) {
        (PathBuf::from(path), size, hash.to_string())
    }

    fn duplicate_group(size: u64, hash: &str, members: &[&str]) -> DuplicateGroup {
        DuplicateGroup {
            size,
            hash: hash.to_string(),
            paths: paths(members),
        }
    }

    #[test]
    fn equal_hashes_group() {
        let files = vec![
            hashed("a", 5, "h"),
            hashed("b", 5, "h"),
            hashed("c", 5, "h"),
        ];

        assert_eq!(
            group_by_size_and_hash(files),
            vec![duplicate_group(5, "h", &["a", "b", "c"])]
        );
    }

    #[test]
    fn equal_size_with_different_hashes_does_not_group() {
        let files = vec![hashed("a", 5, "h1"), hashed("b", 5, "h2")];

        assert!(group_by_size_and_hash(files).is_empty());
    }

    #[test]
    fn groups_are_ordered_by_first_appearance() {
        let files = vec![
            hashed("x/1", 7, "hz"),
            hashed("x/2", 3, "ha"),
            hashed("x/3", 9, "single"),
            hashed("y/1", 3, "ha"),
            hashed("y/2", 7, "hz"),
            hashed("z/1", 7, "hz"),
        ];

        assert_eq!(
            group_by_size_and_hash(files),
            vec![
                duplicate_group(7, "hz", &["x/1", "y/2", "z/1"]),
                duplicate_group(3, "ha", &["x/2", "y/1"]),
            ]
        );
    }

    #[test]
    fn group_ids_are_stable_across_a_rebuild() {
        let files = vec![
            hashed("b", 1, "h2"),
            hashed("a", 2, "h1"),
            hashed("c", 1, "h2"),
            hashed("d", 2, "h1"),
            hashed("e", 3, "h3"),
            hashed("f", 3, "h3"),
        ];

        let first = group_by_size_and_hash(files.clone());

        assert_eq!(first.len(), 3);
        assert_eq!(group_by_size_and_hash(files), first);
    }

    fn two_groups() -> Vec<DuplicateGroup> {
        vec![
            duplicate_group(1, "h1", &["a", "b", "c"]),
            duplicate_group(2, "h2", &["d", "e"]),
        ]
    }

    #[test]
    fn keep_this_sets_the_keeper_of_the_members_group() {
        let groups = two_groups();
        let mut kept = HashMap::new();

        assert!(keep_this(&groups, &mut kept, Path::new("b")));
        assert!(keep_this(&groups, &mut kept, Path::new("e")));

        assert_eq!(kept, keepers(&[("h1", "b"), ("h2", "e")]));
    }

    #[test]
    fn keep_this_replaces_the_groups_keeper() {
        let groups = two_groups();
        let mut kept = keepers(&[("h1", "a")]);

        assert!(keep_this(&groups, &mut kept, Path::new("c")));

        assert_eq!(kept, keepers(&[("h1", "c")]));
    }

    #[test]
    fn keep_this_on_a_non_member_is_refused() {
        let groups = two_groups();
        let mut kept = keepers(&[("h1", "a")]);

        assert!(!keep_this(&groups, &mut kept, Path::new("elsewhere")));
        assert!(!keep_this(&groups, &mut kept, Path::new("")));

        assert_eq!(kept, keepers(&[("h1", "a")]));
    }
}
