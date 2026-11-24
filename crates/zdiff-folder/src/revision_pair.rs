use std::{
    fs, io,
    path::{Path, PathBuf},
};

use zcommon::{
    universal_path::{GitRev, UniversalPath},
    vcs::{git::GitRunner, p4::P4Runner},
};

use crate::{
    external_diff_tool::DiffTool,
    scan::is_plain_name,
    vcs_source::{ContentSource, GitContent, depot_node, read_depot_file, read_git_file},
};

pub struct PairSource<'a> {
    pub content: &'a ContentSource,
    pub path: &'a Path,
}

pub fn is_file_diff_tool(tool: &DiffTool) -> bool {
    tool.exe_path
        .file_stem()
        .is_some_and(|stem| stem.eq_ignore_ascii_case("zdiff-gui"))
}

/// On success the caller removes `temp_dir` with `remove_temp_dir`; on error it is already gone.
pub fn tool_paths(
    git: &impl GitRunner,
    p4: &impl P4Runner,
    tool: &DiffTool,
    sides: [PairSource; 2],
    rel_path: &str,
    temp_dir: &Path,
) -> io::Result<[String; 2]> {
    let paths = fetch_paths(git, p4, tool, sides, rel_path, temp_dir);
    if paths.is_err() {
        remove_temp_dir(temp_dir);
    }
    paths
}

fn fetch_paths(
    git: &impl GitRunner,
    p4: &impl P4Runner,
    tool: &DiffTool,
    sides: [PairSource; 2],
    rel_path: &str,
    temp_dir: &Path,
) -> io::Result<[String; 2]> {
    let revision_paths = is_file_diff_tool(tool);
    let mut paths = [String::new(), String::new()];
    for ((slot, source), side_dir) in paths.iter_mut().zip(sides).zip(["left", "right"]) {
        *slot = match source.content {
            ContentSource::Disk => source.path.to_string_lossy().into_owned(),
            ContentSource::Git(content) if revision_paths => {
                revision_path(content, source.path)?.to_string()
            }
            ContentSource::Git(content) => {
                let file = temp_file(temp_dir, side_dir, rel_path)?;
                write_file(&file, read_git_file(git, content, source.path)?)?;
                file.to_string_lossy().into_owned()
            }
            ContentSource::Depot(content) if revision_paths => {
                let (path, revision) = depot_node(content, source.path)?;
                UniversalPath::Depot(path, Some(revision)).to_string()
            }
            ContentSource::Depot(content) => {
                let file = temp_file(temp_dir, side_dir, rel_path)?;
                write_file(&file, read_depot_file(p4, content, source.path)?)?;
                file.to_string_lossy().into_owned()
            }
        };
    }
    Ok(paths)
}

pub fn remove_temp_dir(dir: &Path) {
    match fs::remove_dir_all(dir) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => {
            log::warn!("removing {}: {err}", dir.display());
        }
        _ => {}
    }
}

fn revision_path(content: &GitContent, node: &Path) -> io::Result<UniversalPath> {
    let rel = node.strip_prefix(&content.model_root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} is not under {}",
                node.display(),
                content.model_root.display()
            ),
        )
    })?;
    let UniversalPath::Git(folder, _) = UniversalPath::from(content.model_root.as_path()) else {
        return Err(io::Error::other(format!(
            "{} is not a revision root",
            content.model_root.display()
        )));
    };
    let rev = match &content.commit {
        Some(commit) => GitRev::Rev(commit.clone()),
        None => GitRev::Index,
    };
    Ok(UniversalPath::Git(folder.join(rel), rev))
}

fn temp_file(temp_dir: &Path, side_dir: &str, rel_path: &str) -> io::Result<PathBuf> {
    if !rel_path.split('/').all(is_plain_name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{rel_path:?} would leave the temp folder"),
        ));
    }
    Ok(rel_path
        .split('/')
        .fold(temp_dir.join(side_dir), |path, segment| path.join(segment)))
}

fn write_file(file: &Path, bytes: Vec<u8>) -> io::Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(file, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_diff_tool::{DiffPair, expand_template, windows_presets};
    use crate::vcs_source::{DepotContent, DepotRevisions};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use zcommon::test_util::os_path;
    use zcommon::vcs::{
        git::{GitError, GitErrorKind},
        p4::FakeP4,
    };

    const COMMIT: &str = "1111111111111111111111111111111111111111";

    struct BlobGit;

    impl GitRunner for BlobGit {
        fn run(&self, _cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
            assert_eq!(args[0], "cat-file");
            Ok(format!("blob {}", args.last().unwrap()).into_bytes())
        }
    }

    fn blob_p4() -> FakeP4<impl Fn(&[&str]) -> Result<String, String>> {
        FakeP4::new(|args| Err(format!("unexpected p4 call: {args:?}")))
    }

    fn git_content(commit: Option<&str>) -> ContentSource {
        ContentSource::Git(GitContent {
            model_root: PathBuf::from(os_path(r"D:\repo\src@git:origin/main")),
            repo_root: PathBuf::from(os_path(r"D:\repo")),
            dir: "src".to_string(),
            commit: commit.map(String::from),
        })
    }

    fn tool(name: &str) -> DiffTool {
        windows_presets(None)
            .into_iter()
            .find(|preset| preset.name == name)
            .unwrap()
            .to_tool(|_| false)
    }

    fn command(tool: &DiffTool, paths: &[String; 2]) -> String {
        let pair = DiffPair {
            left: &paths[0],
            right: &paths[1],
            left_name: "Folder 1: a.txt",
            right_name: "Folder 2: a.txt",
        };
        expand_template(&tool.template, &pair).unwrap()
    }

    fn open(tool: &DiffTool, left: &ContentSource, temp: &Path) -> io::Result<[String; 2]> {
        let left_node = PathBuf::from(os_path(r"D:\repo\src@git:origin/main\sub\a.txt"));
        let right_node = PathBuf::from(os_path(r"D:\wt\src\sub\a.txt"));
        tool_paths(
            &BlobGit,
            &blob_p4(),
            tool,
            [
                PairSource {
                    content: left,
                    path: &left_node,
                },
                PairSource {
                    content: &ContentSource::Disk,
                    path: &right_node,
                },
            ],
            "sub/a.txt",
            temp,
        )
    }

    #[test]
    fn file_diff_tool_gets_revision_paths_pinned_to_the_commit() {
        let temp = tempfile::tempdir().unwrap();
        let tool = tool("zdiff-gui");

        let paths = open(&tool, &git_content(Some(COMMIT)), temp.path()).unwrap();

        assert_eq!(
            command(&tool, &paths),
            format!(
                r#""{}@git:{COMMIT}" "{}" -nl "Folder 1: a.txt" -nr "Folder 2: a.txt""#,
                os_path(r"D:\repo\src\sub\a.txt"),
                os_path(r"D:\wt\src\sub\a.txt")
            )
        );
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn file_diff_tool_gets_the_index_path_for_an_index_side() {
        let temp = tempfile::tempdir().unwrap();

        let paths = open(&tool("zdiff-gui"), &git_content(None), temp.path()).unwrap();

        assert_eq!(paths[0], os_path(r"D:\repo\src\sub\a.txt@git:index"));
    }

    #[test]
    fn other_tools_get_temp_files_holding_the_revision_content() {
        let temp = tempfile::tempdir().unwrap();
        let tool = tool("WinMerge");

        let paths = open(&tool, &git_content(Some(COMMIT)), temp.path()).unwrap();

        let fetched = Path::new(&paths[0]);
        assert!(fetched.starts_with(temp.path()));
        assert_eq!(fetched.file_name().unwrap(), "a.txt");
        assert_eq!(
            fs::read_to_string(fetched).unwrap(),
            format!("blob {COMMIT}:src/sub/a.txt")
        );
        assert_eq!(paths[1], os_path(r"D:\wt\src\sub\a.txt"));
        assert!(command(&tool, &paths).contains(&format!(r#""{}""#, paths[0])));
    }

    #[test]
    fn the_two_sides_fetch_to_different_files() {
        let temp = tempfile::tempdir().unwrap();
        let content = git_content(Some(COMMIT));
        let node = PathBuf::from(os_path(r"D:\repo\src@git:origin/main\a.txt"));
        let side = || PairSource {
            content: &content,
            path: &node,
        };

        let paths = tool_paths(
            &BlobGit,
            &blob_p4(),
            &tool("WinMerge"),
            [side(), side()],
            "a.txt",
            temp.path(),
        )
        .unwrap();

        assert_ne!(paths[0], paths[1]);
        assert!(Path::new(&paths[0]).is_file() && Path::new(&paths[1]).is_file());
    }

    #[test]
    fn a_rel_path_that_leaves_the_temp_dir_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let inner = temp.path().join("inner");
        let content = git_content(Some(COMMIT));
        let left_node = PathBuf::from(os_path(r"D:\repo\src@git:origin/main\a.txt"));
        let right_node = PathBuf::from(os_path(r"D:\wt\src\a.txt"));

        for rel_path in [
            r"a\..\..\..\x.txt",
            "../x.txt",
            "a/../../x.txt",
            "./x.txt",
            "C:x.txt",
            "x.txt:stream",
            "a/.. /x.txt",
        ] {
            let result = tool_paths(
                &BlobGit,
                &blob_p4(),
                &tool("WinMerge"),
                [
                    PairSource {
                        content: &content,
                        path: &left_node,
                    },
                    PairSource {
                        content: &ContentSource::Disk,
                        path: &right_node,
                    },
                ],
                rel_path,
                &inner,
            );

            assert!(result.is_err(), "{rel_path}");
            assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0, "{rel_path}");
        }
    }

    #[test]
    fn a_node_outside_the_side_is_an_error_for_both_kinds_of_tool() {
        let temp = tempfile::tempdir().unwrap();
        let content = git_content(Some(COMMIT));
        let left_node = PathBuf::from(os_path(r"D:\elsewhere\a.txt"));
        let right_node = PathBuf::from(os_path(r"D:\wt\a.txt"));

        for name in ["zdiff-gui", "WinMerge"] {
            let result = tool_paths(
                &BlobGit,
                &blob_p4(),
                &tool(name),
                [
                    PairSource {
                        content: &content,
                        path: &left_node,
                    },
                    PairSource {
                        content: &ContentSource::Disk,
                        path: &right_node,
                    },
                ],
                "a.txt",
                temp.path(),
            );
            assert!(result.is_err(), "{name}");
        }
    }

    fn depot_content(revisions: DepotRevisions) -> ContentSource {
        ContentSource::Depot(DepotContent {
            model_root: PathBuf::from("//depot/proj@50"),
            depot_folder: "//depot/proj".to_string(),
            revisions,
        })
    }

    fn depot_files() -> DepotRevisions {
        DepotRevisions::Files(HashMap::from([("sub/a.txt".to_string(), 7)]))
    }

    fn open_depot(
        tool: &DiffTool,
        left: &ContentSource,
        p4: &impl P4Runner,
        temp: &Path,
    ) -> io::Result<[String; 2]> {
        tool_paths(
            &BlobGit,
            p4,
            tool,
            [
                PairSource {
                    content: left,
                    path: Path::new("//depot/proj@50/sub/a.txt"),
                },
                PairSource {
                    content: &ContentSource::Disk,
                    path: Path::new(r"D:\wt\sub\a.txt"),
                },
            ],
            "sub/a.txt",
            temp,
        )
    }

    #[test]
    fn file_diff_tool_gets_the_depot_path_at_the_listed_revision() {
        let temp = tempfile::tempdir().unwrap();

        let files = open_depot(
            &tool("zdiff-gui"),
            &depot_content(depot_files()),
            &blob_p4(),
            temp.path(),
        )
        .unwrap();
        let shelf = open_depot(
            &tool("zdiff-gui"),
            &depot_content(DepotRevisions::Shelf(99)),
            &blob_p4(),
            temp.path(),
        )
        .unwrap();

        assert_eq!(files[0], "//depot/proj/sub/a.txt#7");
        assert_eq!(shelf[0], "//depot/proj/sub/a.txt@=99");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn other_tools_get_temp_files_printed_from_the_depot() {
        let temp = tempfile::tempdir().unwrap();
        let p4 = FakeP4::new(|args| {
            assert_eq!(args, ["print", "-q", "//depot/proj/sub/a.txt#7"]);
            Ok("depot content".to_string())
        });

        let paths = open_depot(
            &tool("WinMerge"),
            &depot_content(depot_files()),
            &p4,
            temp.path(),
        )
        .unwrap();

        let fetched = Path::new(&paths[0]);
        assert!(fetched.starts_with(temp.path()));
        assert_eq!(fs::read_to_string(fetched).unwrap(), "depot content");
    }

    #[test]
    fn a_depot_node_that_was_not_listed_is_an_error_for_both_kinds_of_tool() {
        let temp = tempfile::tempdir().unwrap();
        let content = depot_content(DepotRevisions::Files(HashMap::new()));

        for name in ["zdiff-gui", "WinMerge"] {
            let result = open_depot(&tool(name), &content, &blob_p4(), temp.path());
            assert!(result.is_err(), "{name}");
        }
    }

    #[test]
    fn removing_the_temp_dir_deletes_the_fetched_files() {
        let root = tempfile::tempdir().unwrap();
        let temp = root.path().join("opened");
        open(&tool("WinMerge"), &git_content(Some(COMMIT)), &temp).unwrap();
        assert!(temp.is_dir());

        remove_temp_dir(&temp);

        assert!(!temp.exists());
        remove_temp_dir(&temp);
    }

    #[test]
    fn a_failed_fetch_reports_the_error_and_leaves_no_directory() {
        struct FailsSecond(std::cell::Cell<u32>);

        impl GitRunner for FailsSecond {
            fn run(&self, _cwd: &Path, _args: &[&str]) -> Result<Vec<u8>, GitError> {
                self.0.set(self.0.get() + 1);
                match self.0.get() {
                    1 => Ok(b"first".to_vec()),
                    _ => Err(GitError {
                        command: "cat-file".to_string(),
                        kind: GitErrorKind::Failed {
                            status: "1".to_string(),
                            stderr: "no such blob".to_string(),
                        },
                    }),
                }
            }
        }

        let root = tempfile::tempdir().unwrap();
        let temp = root.path().join("opened");
        let content = git_content(Some(COMMIT));
        let node = PathBuf::from(os_path(r"D:\repo\src@git:origin/main\a.txt"));
        let side = || PairSource {
            content: &content,
            path: &node,
        };

        let result = tool_paths(
            &FailsSecond(Default::default()),
            &blob_p4(),
            &tool("WinMerge"),
            [side(), side()],
            "a.txt",
            &temp,
        );

        assert!(result.unwrap_err().to_string().contains("no such blob"));
        assert!(!temp.exists());
    }

    #[test]
    fn the_file_diff_tool_is_told_by_its_executable() {
        assert!(is_file_diff_tool(&tool("zdiff-gui")));
        let renamed = DiffTool {
            name: "my viewer".to_string(),
            exe_path: PathBuf::from(os_path(r"C:\tools\ZDiff-GUI.exe")),
            ..tool("zdiff-gui")
        };
        assert!(is_file_diff_tool(&renamed));
        assert!(!is_file_diff_tool(&tool("WinMerge")));
    }
}
