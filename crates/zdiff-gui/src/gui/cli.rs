use zdiff::universal_path::UniversalPath;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    pub path: UniversalPath,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchRequest {
    Diff {
        left: Side,
        right: Side,
    },
    Merge {
        base: Side,
        left: Side,
        right: Side,
        output: Side,
    },
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub request: LaunchRequest,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Base,
    Left,
    Right,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Path(Role),
    Label(Role),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    Twice {
        field: Field,
        first: String,
        second: String,
    },
    MissingValue(String),
    PositionalCount(usize),
    MissingPath(Role),
    Folders {
        left: String,
        right: String,
    },
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Role::Base => "base",
            Role::Left => "left",
            Role::Right => "right",
            Role::Output => "output",
        };
        f.write_str(name)
    }
}

impl std::fmt::Display for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Field::Path(role) => write!(f, "{role} file"),
            Field::Label(role) => write!(f, "{role} label"),
        }
    }
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::Twice {
                field,
                first,
                second,
            } => write!(f, "{field} given twice: `{first}` and `{second}`"),
            LaunchError::MissingValue(flag) => write!(f, "`{flag}` needs a value"),
            LaunchError::PositionalCount(n) => write!(
                f,
                "expected 2 files to diff or 4 to merge (base, left, right, output), got {n}"
            ),
            LaunchError::MissingPath(role) => write!(f, "no {role} file given"),
            LaunchError::Folders { left, right } => write!(
                f,
                "`{left}` and `{right}` are folders; zdiff-gui compares files, use zdiff-folder-gui for folders"
            ),
        }
    }
}

pub const HELP: &str = "\
Usage:
  zdiff-gui LEFT RIGHT                  diff two files
  zdiff-gui BASE LEFT RIGHT OUTPUT      merge (opens LEFT and RIGHT as a diff for now)

Files can be local paths or depot paths (//depot/a.cpp#3).

p4merge labels:
  -nl LABEL   left       -nr LABEL   right
  -nb LABEL   base       -nm LABEL   output
  zdiff-gui -nl \"a.cpp#3\" -nr \"a.cpp (workspace)\" C:\\tmp\\t1.cpp C:\\ws\\a.cpp

git difftool / mergetool:
  zdiff-gui $LOCAL $REMOTE
  zdiff-gui $BASE $LOCAL $REMOTE $MERGED

GNU-style flags (--flag VALUE or --flag=VALUE):
  --left-label  --right-label  --base-label  --base  --output
  zdiff-gui --left-label=old --right-label=new a.txt b.txt
  zdiff-gui --base base.txt --output out.txt left.txt right.txt

TortoiseSVN / TortoiseGit:
  /base:  /mine:  /theirs:  /merged:  with labels /basename:  /minename:  /theirsname:  /mergedname:
  zdiff-gui /base:%base /mine:%mine /basename:%bname /minename:%yname
  zdiff-gui /base:%base /theirs:%theirs /mine:%mine /merged:%merged

Help: -h  --help  /?
Log:  -log  log to the launching terminal, or to a new console window

Labels replace the path in the path bar and window title; the path is in the tooltip.
Unknown flags are ignored with a warning in the log.";

struct Given {
    value: String,
    source: String,
}

#[derive(Default)]
struct Slots([[Option<Given>; 2]; 4]);

impl Slots {
    fn get(&mut self, field: Field) -> &mut Option<Given> {
        match field {
            Field::Path(role) => &mut self.0[role as usize][0],
            Field::Label(role) => &mut self.0[role as usize][1],
        }
    }

    fn set(&mut self, field: Field, given: Given) -> Result<(), LaunchError> {
        let slot = self.get(field);
        if let Some(first) = slot {
            return Err(LaunchError::Twice {
                field,
                first: first.source.clone(),
                second: given.source,
            });
        }
        *slot = Some(given);
        Ok(())
    }

    fn side(&mut self, role: Role) -> Result<Side, LaunchError> {
        let path = self
            .get(Field::Path(role))
            .take()
            .ok_or(LaunchError::MissingPath(role))?;
        Ok(Side {
            path: UniversalPath::new(path.value),
            label: self.get(Field::Label(role)).take().map(|g| g.value),
        })
    }
}

fn gnu_field(name: &str) -> Option<Field> {
    match name {
        "left-label" => Some(Field::Label(Role::Left)),
        "right-label" => Some(Field::Label(Role::Right)),
        "base-label" => Some(Field::Label(Role::Base)),
        "base" => Some(Field::Path(Role::Base)),
        "output" => Some(Field::Path(Role::Output)),
        _ => None,
    }
}

fn p4merge_field(flag: &str) -> Option<Field> {
    match flag {
        "-nl" => Some(Field::Label(Role::Left)),
        "-nr" => Some(Field::Label(Role::Right)),
        "-nb" => Some(Field::Label(Role::Base)),
        "-nm" => Some(Field::Label(Role::Output)),
        _ => None,
    }
}

fn tortoise_field(name: &str) -> Option<Field> {
    match name {
        "base" => Some(Field::Path(Role::Base)),
        "theirs" => Some(Field::Path(Role::Left)),
        "mine" => Some(Field::Path(Role::Right)),
        "merged" => Some(Field::Path(Role::Output)),
        "basename" => Some(Field::Label(Role::Base)),
        "theirsname" => Some(Field::Label(Role::Left)),
        "minename" => Some(Field::Label(Role::Right)),
        "mergedname" => Some(Field::Label(Role::Output)),
        _ => None,
    }
}

fn split_tortoise(arg: &str) -> Option<(&str, &str)> {
    let (name, value) = arg.strip_prefix('/')?.split_once(':')?;
    (!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphabetic())).then_some((name, value))
}

pub fn parse_launch_args<I>(args: I) -> Result<Launch, LaunchError>
where
    I: IntoIterator,
    I::Item: Into<String>,
{
    let mut args = args.into_iter().map(Into::into);
    let mut slots = Slots::default();
    let mut positionals = Vec::new();
    let mut warnings = Vec::new();

    while let Some(arg) = args.next() {
        if matches!(arg.as_str(), "-h" | "--help" | "/?") {
            return Ok(Launch {
                request: LaunchRequest::Help,
                warnings,
            });
        }

        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let Some(field) = gnu_field(name) else {
                warnings.push(format!("ignoring unknown flag `{arg}`"));
                continue;
            };
            let given = match inline {
                Some("") => return Err(LaunchError::MissingValue(arg)),
                Some(value) => Given {
                    value: value.to_string(),
                    source: arg.clone(),
                },
                None => {
                    let value = args.next().ok_or(LaunchError::MissingValue(arg.clone()))?;
                    Given {
                        source: format!("{arg} {value}"),
                        value,
                    }
                }
            };
            slots.set(field, given)?;
        } else if arg.len() > 1 && arg.starts_with('-') {
            let Some(field) = p4merge_field(&arg) else {
                warnings.push(format!("ignoring unknown flag `{arg}`"));
                continue;
            };
            let value = args.next().ok_or(LaunchError::MissingValue(arg.clone()))?;
            slots.set(
                field,
                Given {
                    source: format!("{arg} {value}"),
                    value,
                },
            )?;
        } else if let Some((name, value)) = split_tortoise(&arg) {
            let Some(field) = tortoise_field(name) else {
                warnings.push(format!("ignoring unknown flag `{arg}`"));
                continue;
            };
            if value.is_empty() {
                return Err(LaunchError::MissingValue(arg));
            }
            slots.set(
                field,
                Given {
                    value: value.to_string(),
                    source: arg.clone(),
                },
            )?;
        } else {
            positionals.push(arg);
        }
    }

    let positional_roles: &[Role] = match positionals.len() {
        0 if [Role::Base, Role::Left, Role::Right, Role::Output]
            .iter()
            .all(|&role| slots.get(Field::Path(role)).is_none()) =>
        {
            return Err(LaunchError::PositionalCount(0));
        }
        0 => &[],
        2 => &[Role::Left, Role::Right],
        4 => &[Role::Base, Role::Left, Role::Right, Role::Output],
        n => return Err(LaunchError::PositionalCount(n)),
    };
    for (&role, arg) in positional_roles.iter().zip(positionals) {
        slots.set(
            Field::Path(role),
            Given {
                source: arg.clone(),
                value: arg,
            },
        )?;
    }

    if slots.get(Field::Path(Role::Output)).is_some() {
        return Ok(Launch {
            request: LaunchRequest::Merge {
                base: slots.side(Role::Base)?,
                left: slots.side(Role::Left)?,
                right: slots.side(Role::Right)?,
                output: slots.side(Role::Output)?,
            },
            warnings,
        });
    }

    if slots.get(Field::Path(Role::Base)).is_some() {
        if slots.get(Field::Path(Role::Left)).is_some() {
            return Err(LaunchError::MissingPath(Role::Output));
        }
        // Tortoise's two-file diff: base goes on the left.
        for (from, to) in [
            (Field::Path(Role::Base), Field::Path(Role::Left)),
            (Field::Label(Role::Base), Field::Label(Role::Left)),
        ] {
            if let Some(given) = slots.get(from).take() {
                slots.set(to, given)?;
            }
        }
    }
    for field in [Field::Label(Role::Base), Field::Label(Role::Output)] {
        if let Some(given) = slots.get(field).take() {
            warnings.push(format!("ignoring {field} `{}`: not a merge", given.source));
        }
    }

    let left = slots.side(Role::Left)?;
    let right = slots.side(Role::Right)?;
    if let (Some(l), Some(r)) = (left.path.as_path(), right.path.as_path())
        && l.is_dir()
        && r.is_dir()
    {
        return Err(LaunchError::Folders {
            left: left.path.to_string(),
            right: right.path.to_string(),
        });
    }

    Ok(Launch {
        request: LaunchRequest::Diff { left, right },
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zdiff::universal_path::DepotRev;

    fn parse(args: &[&str]) -> Result<Launch, LaunchError> {
        parse_launch_args(args.iter().copied())
    }

    fn side(path: &str, label: Option<&str>) -> Side {
        Side {
            path: UniversalPath::new(path),
            label: label.map(str::to_string),
        }
    }

    fn diff(left: Side, right: Side) -> LaunchRequest {
        LaunchRequest::Diff { left, right }
    }

    fn merge(base: Side, left: Side, right: Side, output: Side) -> LaunchRequest {
        LaunchRequest::Merge {
            base,
            left,
            right,
            output,
        }
    }

    fn ok(args: &[&str]) -> Launch {
        parse(args).unwrap_or_else(|e| panic!("{args:?} failed: {e}"))
    }

    #[test]
    fn two_positionals_are_a_diff() {
        let launch = ok(&["a.txt", "b.txt"]);
        assert_eq!(
            launch.request,
            diff(side("a.txt", None), side("b.txt", None))
        );
        assert!(launch.warnings.is_empty());
    }

    #[test]
    fn p4merge_labels() {
        let launch = ok(&[
            "-nl",
            "//depot/a.cpp#3",
            "-nr",
            "a.cpp (workspace)",
            "t1",
            "t2",
        ]);
        assert_eq!(
            launch.request,
            diff(
                side("t1", Some("//depot/a.cpp#3")),
                side("t2", Some("a.cpp (workspace)"))
            )
        );
    }

    #[test]
    fn gnu_labels_space_form() {
        let launch = ok(&["--left-label", "L", "a", "--right-label", "R", "b"]);
        assert_eq!(
            launch.request,
            diff(side("a", Some("L")), side("b", Some("R")))
        );
    }

    #[test]
    fn gnu_labels_equals_form() {
        let launch = ok(&["--left-label=L x", "a", "b", "--right-label=R=y"]);
        assert_eq!(
            launch.request,
            diff(side("a", Some("L x")), side("b", Some("R=y")))
        );
    }

    #[test]
    fn tortoise_two_file_form_is_base_left_mine_right() {
        let launch = ok(&[
            "/base:C:\\tmp\\a.svn-base",
            "/mine:C:\\work\\a.cpp",
            "/basename:a.cpp BASE",
            "/minename:a.cpp WORKING",
        ]);
        assert_eq!(
            launch.request,
            diff(
                side("C:\\tmp\\a.svn-base", Some("a.cpp BASE")),
                side("C:\\work\\a.cpp", Some("a.cpp WORKING"))
            )
        );
        assert!(launch.warnings.is_empty());
    }

    #[test]
    fn depot_and_revision_paths_go_through_universal_path() {
        let launch = ok(&["//depot/main/a.cpp#3", "//depot/main/a.cpp"]);
        assert_eq!(
            launch.request,
            diff(
                Side {
                    path: UniversalPath::Depot("//depot/main/a.cpp".into(), Some(DepotRev::Rev(3))),
                    label: None
                },
                Side {
                    path: UniversalPath::Depot("//depot/main/a.cpp".into(), None),
                    label: None
                },
            )
        );

        let launch = ok(&["src/a.cpp@git:HEAD~1", "src/a.cpp"]);
        assert_eq!(
            launch.request,
            diff(side("src/a.cpp@git:HEAD~1", None), side("src/a.cpp", None))
        );
    }

    #[test]
    fn four_positionals_are_base_left_right_output() {
        let launch = ok(&["base", "theirs", "yours", "merged"]);
        assert_eq!(
            launch.request,
            merge(
                side("base", None),
                side("theirs", None),
                side("yours", None),
                side("merged", None)
            )
        );
    }

    #[test]
    fn gnu_base_and_output_with_two_positionals() {
        let launch = ok(&["--base", "b", "l", "r", "--output=o"]);
        assert_eq!(
            launch.request,
            merge(
                side("b", None),
                side("l", None),
                side("r", None),
                side("o", None)
            )
        );
    }

    #[test]
    fn tortoise_four_file_form_is_theirs_left_mine_right() {
        let launch = ok(&[
            "/base:b",
            "/theirs:t",
            "/mine:m",
            "/merged:o",
            "/basename:B",
            "/theirsname:T",
            "/minename:M",
            "/mergedname:O",
        ]);
        assert_eq!(
            launch.request,
            merge(
                side("b", Some("B")),
                side("t", Some("T")),
                side("m", Some("M")),
                side("o", Some("O"))
            )
        );
    }

    #[test]
    fn p4merge_labels_for_every_merge_role() {
        let launch = ok(&[
            "-nb", "B", "-nl", "L", "-nr", "R", "-nm", "O", "b", "l", "r", "o",
        ]);
        assert_eq!(
            launch.request,
            merge(
                side("b", Some("B")),
                side("l", Some("L")),
                side("r", Some("R")),
                side("o", Some("O"))
            )
        );
    }

    #[test]
    fn gnu_labels_for_every_merge_role() {
        let launch = ok(&[
            "--base-label=B",
            "--left-label",
            "L",
            "--right-label=R",
            "--base",
            "b",
            "--output",
            "o",
            "l",
            "r",
        ]);
        assert_eq!(
            launch.request,
            merge(
                side("b", Some("B")),
                side("l", Some("L")),
                side("r", Some("R")),
                side("o", None)
            )
        );
    }

    #[test]
    fn three_positionals_are_an_error() {
        assert_eq!(
            parse(&["a", "b", "c"]),
            Err(LaunchError::PositionalCount(3))
        );
    }

    #[test]
    fn one_positional_is_an_error() {
        assert_eq!(parse(&["a"]), Err(LaunchError::PositionalCount(1)));
    }

    #[test]
    fn label_twice_in_two_styles_names_both() {
        let err = parse(&["-nl", "A", "--left-label=B", "a", "b"]).unwrap_err();
        assert_eq!(
            err,
            LaunchError::Twice {
                field: Field::Label(Role::Left),
                first: "-nl A".into(),
                second: "--left-label=B".into(),
            }
        );
        let message = err.to_string();
        assert!(message.contains("-nl A"), "{message}");
        assert!(message.contains("--left-label=B"), "{message}");
    }

    #[test]
    fn output_twice_in_two_styles_is_an_error() {
        assert_eq!(
            parse(&["--output", "o1", "b", "l", "r", "o2"]),
            Err(LaunchError::Twice {
                field: Field::Path(Role::Output),
                first: "--output o1".into(),
                second: "o2".into(),
            })
        );
        assert_eq!(
            parse(&[
                "/merged:o1",
                "--output=o2",
                "/base:b",
                "/theirs:t",
                "/mine:m"
            ]),
            Err(LaunchError::Twice {
                field: Field::Path(Role::Output),
                first: "/merged:o1".into(),
                second: "--output=o2".into(),
            })
        );
    }

    #[test]
    fn base_without_output_is_an_error() {
        assert_eq!(
            parse(&["--base", "b", "l", "r"]),
            Err(LaunchError::MissingPath(Role::Output))
        );
    }

    #[test]
    fn two_folders_are_an_error_pointing_to_the_folder_app() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        let l = left.path().to_str().unwrap();
        let r = right.path().to_str().unwrap();
        let err = parse(&[l, r]).unwrap_err();
        assert!(matches!(err, LaunchError::Folders { .. }), "{err:?}");
        assert!(err.to_string().contains("zdiff-folder-gui"), "{err}");
    }

    #[test]
    fn one_folder_is_not_the_folder_error() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_str().unwrap();
        assert!(parse(&[d, "missing.txt"]).is_ok());
    }

    #[test]
    fn flag_missing_its_value_is_an_error() {
        assert_eq!(
            parse(&["a", "b", "-nl"]),
            Err(LaunchError::MissingValue("-nl".into()))
        );
        assert_eq!(
            parse(&["a", "b", "--right-label"]),
            Err(LaunchError::MissingValue("--right-label".into()))
        );
        assert_eq!(
            parse(&["a", "b", "--output="]),
            Err(LaunchError::MissingValue("--output=".into()))
        );
        assert_eq!(
            parse(&["/base:", "/mine:m"]),
            Err(LaunchError::MissingValue("/base:".into()))
        );
    }

    #[test]
    fn unknown_flags_warn_and_are_ignored() {
        for flag in ["-text", "--foo", "--foo=bar", "/foo:bar"] {
            let launch = ok(&[flag, "a", "b"]);
            assert_eq!(
                launch.request,
                diff(side("a", None), side("b", None)),
                "{flag}"
            );
            assert_eq!(launch.warnings.len(), 1, "{flag}: {:?}", launch.warnings);
            assert!(launch.warnings[0].contains(flag), "{:?}", launch.warnings);
        }
    }

    #[test]
    fn merge_labels_on_a_diff_warn_and_are_ignored() {
        let launch = ok(&["-nb", "B", "a", "b", "-nm", "M"]);
        assert_eq!(launch.request, diff(side("a", None), side("b", None)));
        assert_eq!(launch.warnings.len(), 2, "{:?}", launch.warnings);
        assert!(
            launch.warnings[0].contains("-nb B"),
            "{:?}",
            launch.warnings
        );
        assert!(
            launch.warnings[1].contains("-nm M"),
            "{:?}",
            launch.warnings
        );
    }

    #[test]
    fn slash_prefixed_path_is_a_path() {
        let launch = ok(&["/home/me/a.txt", "/tmp/b.txt"]);
        assert_eq!(
            launch.request,
            diff(side("/home/me/a.txt", None), side("/tmp/b.txt", None))
        );
        assert!(launch.warnings.is_empty());
    }

    #[test]
    fn help_flags() {
        for flag in ["-h", "--help", "/?"] {
            assert_eq!(ok(&[flag]).request, LaunchRequest::Help, "{flag}");
            assert_eq!(
                ok(&["a", flag, "b", "c"]).request,
                LaunchRequest::Help,
                "{flag}"
            );
        }
    }
}
