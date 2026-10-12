use std::{fs, io, path::Path};

use zcommon::{
    universal_path::{GitRev, UniversalPath},
    vcs::git::GitRunner,
};

use crate::{
    external_diff_tool::DiffTool,
    vcs_source::{ContentSource, GitContent, read_git_file},
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

/// The file diff gets `path@git:<commit>` for a revision side, which it reads itself; any other
/// tool gets the blob written under `temp_dir/{left,right}/<rel_path>`. Nothing is removed
/// afterwards, the tool opens the files after this returns.
pub fn tool_paths(
    git: &impl GitRunner,
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
                let file = rel_path
                    .split('/')
                    .fold(temp_dir.join(side_dir), |path, segment| path.join(segment));
                fetch(git, content, source.path, &file)?;
                file.to_string_lossy().into_owned()
            }
        };
    }
    Ok(paths)
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

fn fetch(git: &impl GitRunner, content: &GitContent, node: &Path, file: &Path) -> io::Result<()> {
    let bytes = read_git_file(git, content, node)?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(file, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_diff_tool::{DiffPair, expand_template, presets};
    use std::path::PathBuf;
    use zcommon::vcs::git::GitError;

    const COMMIT: &str = "1111111111111111111111111111111111111111";

    struct BlobGit;

    impl GitRunner for BlobGit {
        fn run(&self, _cwd: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
            assert_eq!(args[0], "cat-file");
            Ok(format!("blob {}", args.last().unwrap()).into_bytes())
        }
    }

    fn git_content(commit: Option<&str>) -> ContentSource {
        ContentSource::Git(GitContent {
            model_root: PathBuf::from(r"D:\repo\src@git:origin/main"),
            repo_root: PathBuf::from(r"D:\repo"),
            dir: "src".to_string(),
            commit: commit.map(String::from),
        })
    }

    fn tool(name: &str) -> DiffTool {
        presets(None)
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
        tool_paths(
            &BlobGit,
            tool,
            [
                PairSource {
                    content: left,
                    path: Path::new(r"D:\repo\src@git:origin/main\sub\a.txt"),
                },
                PairSource {
                    content: &ContentSource::Disk,
                    path: Path::new(r"D:\wt\src\sub\a.txt"),
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
                r#""D:\repo\src\sub\a.txt@git:{COMMIT}" "D:\wt\src\sub\a.txt" -nl "Folder 1: a.txt" -nr "Folder 2: a.txt""#
            )
        );
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn file_diff_tool_gets_the_index_path_for_an_index_side() {
        let temp = tempfile::tempdir().unwrap();

        let paths = open(&tool("zdiff-gui"), &git_content(None), temp.path()).unwrap();

        assert_eq!(paths[0], r"D:\repo\src\sub\a.txt@git:index");
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
        assert_eq!(paths[1], r"D:\wt\src\sub\a.txt");
        assert!(command(&tool, &paths).contains(&format!(r#""{}""#, paths[0])));
    }

    #[test]
    fn the_two_sides_fetch_to_different_files() {
        let temp = tempfile::tempdir().unwrap();
        let content = git_content(Some(COMMIT));
        let side = |path| PairSource {
            content: &content,
            path: Path::new(path),
        };

        let paths = tool_paths(
            &BlobGit,
            &tool("WinMerge"),
            [
                side(r"D:\repo\src@git:origin/main\a.txt"),
                side(r"D:\repo\src@git:origin/main\a.txt"),
            ],
            "a.txt",
            temp.path(),
        )
        .unwrap();

        assert_ne!(paths[0], paths[1]);
        assert!(Path::new(&paths[0]).is_file() && Path::new(&paths[1]).is_file());
    }

    #[test]
    fn a_node_outside_the_side_is_an_error_for_both_kinds_of_tool() {
        let temp = tempfile::tempdir().unwrap();
        let content = git_content(Some(COMMIT));

        for name in ["zdiff-gui", "WinMerge"] {
            let result = tool_paths(
                &BlobGit,
                &tool(name),
                [
                    PairSource {
                        content: &content,
                        path: Path::new(r"D:\elsewhere\a.txt"),
                    },
                    PairSource {
                        content: &ContentSource::Disk,
                        path: Path::new(r"D:\wt\a.txt"),
                    },
                ],
                "a.txt",
                temp.path(),
            );
            assert!(result.is_err(), "{name}");
        }
    }

    #[test]
    fn the_file_diff_tool_is_told_by_its_executable() {
        assert!(is_file_diff_tool(&tool("zdiff-gui")));
        let renamed = DiffTool {
            name: "my viewer".to_string(),
            exe_path: PathBuf::from(r"C:\tools\ZDiff-GUI.exe"),
            ..tool("zdiff-gui")
        };
        assert!(is_file_diff_tool(&renamed));
        assert!(!is_file_diff_tool(&tool("WinMerge")));
    }
}
