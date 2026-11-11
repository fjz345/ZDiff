use std::env;
use std::fmt;
use std::io;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command};
use zcommon::kinds::{FileKind, KindTable, normalize_extension};

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DiffToolDefaultArgs {
    pub default_args: Vec<String>,
}

impl DiffToolDefaultArgs {
    pub fn to_string(&self) -> String {
        self.default_args.join("\n")
    }

    pub fn from_string(input: &str) -> Self {
        let default_args = input
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();

        Self { default_args }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LegacyDiffToolConfig {
    pub exe_path: PathBuf,
    pub prefix_args: DiffToolDefaultArgs,
    pub diff_path_1_args: String,
    pub diff_path_2_args: String,
    pub suffix_args: DiffToolDefaultArgs,
}

impl LegacyDiffToolConfig {
    pub fn default_zdiff() -> Self {
        Self {
            exe_path: PathBuf::from(r"zdiff-gui.exe"),
            prefix_args: DiffToolDefaultArgs::from_string(""),
            diff_path_1_args: String::from(r#"{}"#),
            diff_path_2_args: String::from(r#"{}"#),
            suffix_args: DiffToolDefaultArgs::from_string(""),
        }
    }
    pub fn default_tortoise() -> Self {
        Self {
            exe_path: PathBuf::from(r"C:\Program Files\TortoiseSVN\bin\TortoiseProc.exe"),
            prefix_args: DiffToolDefaultArgs::from_string("/command:diff"),
            diff_path_1_args: String::from(r#"/path:"{}""#),
            diff_path_2_args: String::from(r#"/path2:"{}""#),
            suffix_args: DiffToolDefaultArgs::from_string("/closeonend:1"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DiffTool {
    pub name: String,
    pub exe_path: PathBuf,
    pub template: String,
    pub kinds: Vec<FileKind>,
}

impl From<LegacyDiffToolConfig> for DiffTool {
    fn from(legacy: LegacyDiffToolConfig) -> Self {
        let quoted = |args: &DiffToolDefaultArgs| {
            args.default_args
                .iter()
                .map(|arg| quote_like_command_arg(arg))
                .collect::<Vec<_>>()
        };
        let parts: Vec<String> = quoted(&legacy.prefix_args)
            .into_iter()
            .chain([
                legacy.diff_path_1_args.replace("{}", "{left}"),
                legacy.diff_path_2_args.replace("{}", "{right}"),
            ])
            .chain(quoted(&legacy.suffix_args))
            .filter(|part| !part.is_empty())
            .collect();
        let name = legacy.exe_path.file_stem().map_or_else(
            || "Diff tool".to_string(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        Self {
            name,
            exe_path: legacy.exe_path,
            template: parts.join(" "),
            kinds: FileKind::ALL.to_vec(),
        }
    }
}

/// The quoting std's Windows `Command::arg` applies, which the legacy prefix and suffix args got.
fn quote_like_command_arg(arg: &str) -> String {
    let quote = arg.is_empty() || arg.contains([' ', '\t']);
    let mut quoted = String::with_capacity(arg.len() + 2);
    if quote {
        quoted.push('"');
    }
    let mut backslashes = 0;
    for c in arg.chars() {
        if c == '\\' {
            backslashes += 1;
        } else {
            if c == '"' {
                quoted.extend(std::iter::repeat_n('\\', backslashes + 1));
            }
            backslashes = 0;
        }
        quoted.push(c);
    }
    if quote {
        quoted.extend(std::iter::repeat_n('\\', backslashes));
        quoted.push('"');
    }
    quoted
}

pub struct DiffToolPreset {
    pub name: &'static str,
    pub install_paths: Vec<PathBuf>,
    pub bare_name: &'static str,
    pub template: &'static str,
    pub kinds: Vec<FileKind>,
}

/// Exhaustive, so a new kind needs a decision here.
fn zdiff_gui_handles(kind: FileKind) -> bool {
    match kind {
        FileKind::Text | FileKind::Image | FileKind::DebugSymbols => true,
        FileKind::Video
        | FileKind::Audio
        | FileKind::Model
        | FileKind::Archive
        | FileKind::Executable
        | FileKind::Intermediate
        | FileKind::UnrealAsset
        | FileKind::UnrealMap
        | FileKind::Binary => false,
    }
}

pub fn presets(local_app_data: Option<&Path>) -> Vec<DiffToolPreset> {
    let paths = |paths: &[&str]| paths.iter().map(PathBuf::from).collect::<Vec<_>>();
    let vs_code_user = local_app_data
        .map(|dir| dir.join(r"Programs\Microsoft VS Code\Code.exe"))
        .into_iter();
    vec![
        DiffToolPreset {
            name: "zdiff-gui",
            install_paths: Vec::new(),
            bare_name: "zdiff-gui.exe",
            template: r#""{left}" "{right}" -nl "{left_name}" -nr "{right_name}""#,
            kinds: FileKind::ALL
                .into_iter()
                .filter(|&kind| zdiff_gui_handles(kind))
                .collect(),
        },
        DiffToolPreset {
            name: "TortoiseMerge",
            install_paths: paths(&[r"C:\Program Files\TortoiseSVN\bin\TortoiseMerge.exe"]),
            bare_name: "TortoiseMerge.exe",
            template: r#"/base:"{left}" /mine:"{right}" /basename:"{left_name}" /minename:"{right_name}""#,
            kinds: vec![FileKind::Text],
        },
        DiffToolPreset {
            name: "WinMerge",
            install_paths: paths(&[r"C:\Program Files\WinMerge\WinMergeU.exe"]),
            bare_name: "WinMergeU.exe",
            template: r#"/e /u /dl "{left_name}" /dr "{right_name}" "{left}" "{right}""#,
            kinds: vec![FileKind::Text],
        },
        DiffToolPreset {
            name: "Beyond Compare",
            install_paths: paths(&[
                r"C:\Program Files\Beyond Compare 5\BCompare.exe",
                r"C:\Program Files\Beyond Compare 4\BCompare.exe",
            ]),
            bare_name: "BCompare.exe",
            template: r#""{left}" "{right}" /lefttitle="{left_name}" /righttitle="{right_name}""#,
            kinds: vec![FileKind::Text, FileKind::Image],
        },
        DiffToolPreset {
            name: "p4merge",
            install_paths: paths(&[r"C:\Program Files\Perforce\p4merge.exe"]),
            bare_name: "p4merge.exe",
            template: r#"-nl "{left_name}" -nr "{right_name}" "{left}" "{right}""#,
            kinds: vec![FileKind::Text],
        },
        DiffToolPreset {
            name: "VS Code",
            install_paths: vs_code_user
                .chain(paths(&[r"C:\Program Files\Microsoft VS Code\Code.exe"]))
                .collect(),
            // The PATH entry is VS Code's bin folder, which holds code.cmd (and a shell script named code).
            bare_name: "code.cmd",
            template: r#"--diff "{left}" "{right}""#,
            kinds: vec![FileKind::Text],
        },
        DiffToolPreset {
            name: "Meld",
            install_paths: paths(&[
                r"C:\Program Files\Meld\Meld.exe",
                r"C:\Program Files (x86)\Meld\Meld.exe",
            ]),
            bare_name: "Meld.exe",
            template: r#"--label "{left_name}" --label "{right_name}" "{left}" "{right}""#,
            kinds: vec![FileKind::Text],
        },
    ]
}

impl DiffToolPreset {
    /// Falls back to the bare name, which `resolve_exe` looks up next to the app and on PATH.
    pub fn to_tool(&self, is_file: impl Fn(&Path) -> bool) -> DiffTool {
        let exe_path = self
            .install_paths
            .iter()
            .find(|path| is_file(path))
            .cloned()
            .unwrap_or_else(|| PathBuf::from(self.bare_name));
        DiffTool {
            name: self.name.to_string(),
            exe_path,
            template: self.template.to_string(),
            kinds: self.kinds.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolNameError {
    Empty,
    Taken(String),
}

impl fmt::Display for ToolNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolNameError::Empty => write!(f, "name is empty"),
            ToolNameError::Taken(name) => write!(f, "a tool named {name:?} already exists"),
        }
    }
}

impl std::error::Error for ToolNameError {}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DiffToolMatch {
    Kind(FileKind),
    Extension(String),
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DiffToolRule {
    pub matches: DiffToolMatch,
    pub tool: String,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(from = "PersistedDiffToolConfig")
)]
pub struct DiffToolConfig {
    pub tools: Vec<DiffTool>,
    pub rules: Vec<DiffToolRule>,
    pub default_tool: String,
    pub mark_undiffable: bool,
}

#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum PersistedDiffToolConfig {
    Current {
        tools: Vec<DiffTool>,
        rules: Vec<DiffToolRule>,
        default_tool: String,
        #[serde(default)]
        mark_undiffable: bool,
    },
    Legacy(LegacyDiffToolConfig),
}

#[cfg(feature = "serde")]
impl From<PersistedDiffToolConfig> for DiffToolConfig {
    fn from(persisted: PersistedDiffToolConfig) -> Self {
        match persisted {
            PersistedDiffToolConfig::Current {
                tools,
                rules,
                default_tool,
                mark_undiffable,
            } => Self {
                tools,
                rules,
                default_tool,
                mark_undiffable,
            },
            PersistedDiffToolConfig::Legacy(legacy) => legacy.into(),
        }
    }
}

impl Default for DiffToolConfig {
    fn default() -> Self {
        Self::from_tool(presets(None).remove(0).to_tool(|_| false))
    }
}

impl From<LegacyDiffToolConfig> for DiffToolConfig {
    fn from(legacy: LegacyDiffToolConfig) -> Self {
        Self::from_tool(legacy.into())
    }
}

impl DiffToolConfig {
    pub fn from_tool(tool: DiffTool) -> Self {
        Self {
            default_tool: tool.name.clone(),
            tools: vec![tool],
            rules: Vec::new(),
            mark_undiffable: false,
        }
    }

    pub fn tool(&self, name: &str) -> Option<&DiffTool> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    pub fn tool_mut(&mut self, name: &str) -> Option<&mut DiffTool> {
        self.tools.iter_mut().find(|tool| tool.name == name)
    }

    /// Renames on a clash ("WinMerge 2"), since rules and the default refer to tools by name.
    pub fn add_tool(&mut self, mut tool: DiffTool) -> String {
        let base = tool.name.clone();
        let mut number = 1;
        while self.tool(&tool.name).is_some() {
            number += 1;
            tool.name = format!("{base} {number}");
        }
        let name = tool.name.clone();
        self.tools.push(tool);
        name
    }

    pub fn rename_tool(&mut self, old: &str, new: &str) -> Result<(), ToolNameError> {
        let new = new.trim();
        if new.is_empty() {
            return Err(ToolNameError::Empty);
        }
        if new != old && self.tool(new).is_some() {
            return Err(ToolNameError::Taken(new.to_string()));
        }
        let tool = self.tool_mut(old).expect("renamed tool exists");
        tool.name = new.to_string();
        for rule in self.rules.iter_mut().filter(|rule| rule.tool == old) {
            rule.tool = new.to_string();
        }
        if self.default_tool == old {
            self.default_tool = new.to_string();
        }
        Ok(())
    }

    /// Drops the rules naming it too; a removed default passes to the first remaining tool.
    pub fn remove_tool(&mut self, name: &str) {
        self.tools.retain(|tool| tool.name != name);
        self.rules.retain(|rule| rule.tool != name);
        if self.default_tool == name {
            self.default_tool = self
                .tools
                .first()
                .map(|tool| tool.name.clone())
                .unwrap_or_default();
        }
    }

    /// First rule whose tool exists wins, else the default tool; `None` if that is missing too.
    pub fn choose_tool(&self, kinds: &KindTable, path: &Path) -> Option<&DiffTool> {
        self.choose_tool_logged(kinds, path, true)
    }

    /// Without the warning for rules naming a missing tool, for callers that run every frame.
    pub fn choose_tool_quietly(&self, kinds: &KindTable, path: &Path) -> Option<&DiffTool> {
        self.choose_tool_logged(kinds, path, false)
    }

    fn choose_tool_logged(&self, kinds: &KindTable, path: &Path, log: bool) -> Option<&DiffTool> {
        let extension = path
            .extension()
            .map(|extension| extension.to_string_lossy())
            .unwrap_or_default();
        let kind = kinds.kind(&extension);
        let extension = normalize_extension(&extension);
        self.rules
            .iter()
            .filter(|rule| match &rule.matches {
                DiffToolMatch::Kind(rule_kind) => *rule_kind == kind,
                DiffToolMatch::Extension(rule_extension) => {
                    normalize_extension(rule_extension) == extension
                }
            })
            .find_map(|rule| {
                let tool = self.tool(&rule.tool);
                if log && tool.is_none() {
                    log::warn!("diff tool rule {rule:?} names a missing tool, skipped");
                }
                tool
            })
            .or_else(|| self.tool(&self.default_tool))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CantDiff {
    pub tool: String,
    pub kinds: [FileKind; 2],
}

impl fmt::Display for CantDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [left, right] = self.kinds;
        if left == right {
            write!(f, "{} can't diff {left:?} files", self.tool)
        } else {
            write!(f, "{} can't diff {left:?} against {right:?}", self.tool)
        }
    }
}

/// A `None` kind is a folder or a missing side, which is never marked.
pub fn cant_diff(kinds: [Option<FileKind>; 2], tool: &DiffTool) -> Option<CantDiff> {
    let [Some(left), Some(right)] = kinds else {
        return None;
    };
    if tool.kinds.contains(&left) && tool.kinds.contains(&right) {
        return None;
    }
    Some(CantDiff {
        tool: tool.name.clone(),
        kinds: [left, right],
    })
}

#[derive(Clone, Copy, Debug)]
pub struct DiffPair<'a> {
    pub left: &'a str,
    pub right: &'a str,
    pub left_name: &'a str,
    pub right_name: &'a str,
}

pub fn folder_labels(rel_path: &str) -> (String, String) {
    (
        format!("Folder 1: {rel_path}"),
        format!("Folder 2: {rel_path}"),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateError {
    UnknownPlaceholder(String),
    UnclosedPlaceholder,
    MissingLeft,
    MissingRight,
    QuoteInValue(String),
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateError::UnknownPlaceholder(name) => write!(f, "unknown placeholder {{{name}}}"),
            TemplateError::UnclosedPlaceholder => write!(f, "'{{' without a closing '}}'"),
            TemplateError::MissingLeft => write!(f, "missing {{left}}"),
            TemplateError::MissingRight => write!(f, "missing {{right}}"),
            TemplateError::QuoteInValue(value) => write!(f, "{value:?} contains a double quote"),
        }
    }
}

impl std::error::Error for TemplateError {}

/// Single pass, so placeholder text inside a value (a file named `{right}.txt`) stays literal.
pub fn expand_template(template: &str, pair: &DiffPair) -> Result<String, TemplateError> {
    let mut expanded = String::with_capacity(template.len() + pair.left.len() + pair.right.len());
    let (mut has_left, mut has_right) = (false, false);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        expanded.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let close = after_open
            .find('}')
            .ok_or(TemplateError::UnclosedPlaceholder)?;
        let value = match &after_open[..close] {
            "left" => {
                has_left = true;
                pair.left
            }
            "right" => {
                has_right = true;
                pair.right
            }
            "left_name" => pair.left_name,
            "right_name" => pair.right_name,
            unknown => return Err(TemplateError::UnknownPlaceholder(unknown.to_string())),
        };
        // Values go inside the template's quotes, so a quote would end them and start new arguments.
        if value.contains('"') {
            return Err(TemplateError::QuoteInValue(value.to_string()));
        }
        expanded.push_str(value);
        rest = &after_open[close + 1..];
    }
    expanded.push_str(rest);
    if !has_left {
        return Err(TemplateError::MissingLeft);
    }
    if !has_right {
        return Err(TemplateError::MissingRight);
    }
    Ok(expanded)
}

pub const SAMPLE_PAIR: DiffPair<'static> = DiffPair {
    left: r"C:\Temp\Left Folder\sample.txt",
    right: r"C:\Temp\Right Folder\sample.txt",
    left_name: "Folder 1: sub/sample.txt",
    right_name: "Folder 2: sub/sample.txt",
};

pub fn preview_command(tool: &DiffTool) -> Result<String, TemplateError> {
    let args = expand_template(&tool.template, &SAMPLE_PAIR)?;
    Ok(format!("\"{}\" {args}", tool.exe_path.display()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExeNotFound {
    pub exe: PathBuf,
    pub searched: Vec<String>,
}

impl fmt::Display for ExeNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} not found, searched: {}",
            self.exe.display(),
            self.searched.join(", ")
        )
    }
}

impl std::error::Error for ExeNotFound {}

pub fn resolve_exe(
    exe: &Path,
    app_dir: Option<&Path>,
    build_time_zdiff: Option<&Path>,
    is_file: impl Fn(&Path) -> bool,
    on_path: impl Fn(&Path) -> Option<PathBuf>,
) -> Result<PathBuf, ExeNotFound> {
    let mut components = exe.components();
    let is_bare = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    );
    if !is_bare {
        return Ok(exe.to_path_buf());
    }

    let is_zdiff = |build_path: &&Path| {
        build_path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(exe))
    };
    let candidates = app_dir
        .map(|dir| dir.join(exe))
        .into_iter()
        .chain(build_time_zdiff.filter(is_zdiff).map(Path::to_path_buf));
    let mut searched = Vec::new();
    for candidate in candidates {
        if is_file(&candidate) {
            return Ok(candidate);
        }
        searched.push(candidate.display().to_string());
    }
    if let Some(found) = on_path(exe) {
        return Ok(found);
    }
    searched.push("PATH".to_string());
    Err(ExeNotFound {
        exe: exe.to_path_buf(),
        searched,
    })
}

/// Also tries `<name>.exe` on Windows, which `Command::new` would have found for an extensionless name.
pub fn find_on_path(name: &Path) -> Option<PathBuf> {
    let mut names = vec![name.to_path_buf()];
    if cfg!(windows) && name.extension().is_none() {
        names.push(name.with_extension("exe"));
    }
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

#[cfg(target_os = "windows")]
pub fn open_diff_tool_windows(exe: &Path, tool: &DiffTool, pair: &DiffPair) -> io::Result<Child> {
    let args = expand_template(&tool.template, pair)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let mut cmd = Command::new(exe);
    // !!! Important !!! use raw_arg.
    cmd.raw_arg(args);

    log::info!("Opening diff tool {:?}", cmd);
    cmd.spawn()
}
pub fn open_diff_tool(exe: &Path, tool: &DiffTool, pair: &DiffPair) -> io::Result<Child> {
    #[cfg(target_os = "windows")]
    return open_diff_tool_windows(exe, tool, pair);

    #[cfg(not(target_os = "windows"))]
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: &str = r"C:\left\sub\a.txt";
    const RIGHT: &str = r"D:\right\sub\a.txt";

    fn pair<'a>(left: &'a str, right: &'a str) -> DiffPair<'a> {
        DiffPair {
            left,
            right,
            left_name: "Folder 1: sub/a.txt",
            right_name: "Folder 2: sub/a.txt",
        }
    }

    fn tool(name: &str) -> DiffTool {
        DiffTool {
            name: name.to_string(),
            exe_path: PathBuf::from(format!("{name}.exe")),
            template: "{left} {right}".to_string(),
            kinds: Vec::new(),
        }
    }

    fn rule(matches: DiffToolMatch, tool: &str) -> DiffToolRule {
        DiffToolRule {
            matches,
            tool: tool.to_string(),
        }
    }

    fn config(rules: Vec<DiffToolRule>) -> DiffToolConfig {
        DiffToolConfig {
            tools: vec![tool("default"), tool("first"), tool("second")],
            rules,
            default_tool: "default".to_string(),
            mark_undiffable: false,
        }
    }

    fn chosen(config: &DiffToolConfig, kinds: &KindTable, path: &str) -> Option<String> {
        config
            .choose_tool(kinds, Path::new(path))
            .map(|tool| tool.name.clone())
    }

    #[test]
    fn template_replaces_every_placeholder() {
        let expanded = expand_template(
            r#""{left}" "{right}" -nl "{left_name}" -nr "{right_name}""#,
            &pair(LEFT, RIGHT),
        );
        assert_eq!(
            expanded.unwrap(),
            r#""C:\left\sub\a.txt" "D:\right\sub\a.txt" -nl "Folder 1: sub/a.txt" -nr "Folder 2: sub/a.txt""#
        );
    }

    #[test]
    fn template_inserts_paths_with_spaces_bare_inside_its_quotes() {
        let expanded = expand_template(
            r#"/base:"{left}" /mine:"{right}""#,
            &pair(r"C:\My Files\a b.txt", r"D:\Other Dir\a b.txt"),
        );
        assert_eq!(
            expanded.unwrap(),
            r#"/base:"C:\My Files\a b.txt" /mine:"D:\Other Dir\a b.txt""#
        );
    }

    #[test]
    fn template_does_not_expand_placeholders_inside_values() {
        let values = DiffPair {
            left_name: "Folder 1: {right}.txt",
            ..pair(LEFT, RIGHT)
        };
        let expanded = expand_template("{left} {right} {left_name}", &values);
        assert_eq!(
            expanded.unwrap(),
            r"C:\left\sub\a.txt D:\right\sub\a.txt Folder 1: {right}.txt"
        );
    }

    #[test]
    fn template_rejects_an_unknown_placeholder() {
        let expanded = expand_template("{left} {middle} {right}", &pair(LEFT, RIGHT));
        assert_eq!(
            expanded,
            Err(TemplateError::UnknownPlaceholder("middle".to_string()))
        );
    }

    #[test]
    fn template_rejects_a_missing_left_or_right() {
        let values = pair(LEFT, RIGHT);
        assert_eq!(
            expand_template("{right} {left_name}", &values),
            Err(TemplateError::MissingLeft)
        );
        assert_eq!(
            expand_template("{left} {right_name}", &values),
            Err(TemplateError::MissingRight)
        );
    }

    #[test]
    fn template_rejects_a_value_with_a_double_quote() {
        let template = r#""{left}" "{right}" -nl "{left_name}""#;
        let escaping = r#"a" & calc & ".txt"#;
        assert_eq!(
            expand_template(template, &pair(escaping, RIGHT)),
            Err(TemplateError::QuoteInValue(escaping.to_string()))
        );
        let values = DiffPair {
            left_name: "Folder 1: \"",
            ..pair(LEFT, RIGHT)
        };
        assert_eq!(
            expand_template(template, &values),
            Err(TemplateError::QuoteInValue("Folder 1: \"".to_string()))
        );
    }

    #[test]
    fn template_rejects_an_unclosed_placeholder() {
        assert_eq!(
            expand_template("{left} {right} {left_name", &pair(LEFT, RIGHT)),
            Err(TemplateError::UnclosedPlaceholder)
        );
    }

    #[test]
    fn folder_labels_name_both_folders_and_the_relative_path() {
        assert_eq!(
            folder_labels("sub/a.txt"),
            (
                "Folder 1: sub/a.txt".to_string(),
                "Folder 2: sub/a.txt".to_string()
            )
        );
    }

    #[test]
    fn migrated_zdiff_config_expands_to_todays_command_line() {
        let tool = DiffTool::from(LegacyDiffToolConfig::default_zdiff());
        assert_eq!(tool.exe_path, PathBuf::from("zdiff-gui.exe"));
        assert_eq!(
            expand_template(&tool.template, &pair(LEFT, RIGHT)).unwrap(),
            r"C:\left\sub\a.txt D:\right\sub\a.txt"
        );
    }

    #[test]
    fn migrated_tortoise_config_expands_to_todays_command_line() {
        let tool = DiffTool::from(LegacyDiffToolConfig::default_tortoise());
        assert_eq!(
            tool.exe_path,
            PathBuf::from(r"C:\Program Files\TortoiseSVN\bin\TortoiseProc.exe")
        );
        assert_eq!(
            expand_template(&tool.template, &pair(r"C:\a b\x.txt", r"C:\c\x.txt")).unwrap(),
            r#"/command:diff /path:"C:\a b\x.txt" /path2:"C:\c\x.txt" /closeonend:1"#
        );
    }

    #[test]
    fn migrated_extra_args_keep_the_quoting_command_arg_gave_them() {
        let legacy = LegacyDiffToolConfig {
            exe_path: PathBuf::from("tool.exe"),
            prefix_args: DiffToolDefaultArgs::from_string("/title:My Diff\n/q:\"x\""),
            diff_path_1_args: "{}".to_string(),
            diff_path_2_args: "-r {} -r2 {}".to_string(),
            suffix_args: DiffToolDefaultArgs::from_string(r"C:\out dir\"),
        };
        let tool = DiffTool::from(legacy);
        assert_eq!(
            expand_template(&tool.template, &pair(LEFT, RIGHT)).unwrap(),
            r#""/title:My Diff" /q:\"x\" C:\left\sub\a.txt -r D:\right\sub\a.txt -r2 D:\right\sub\a.txt "C:\out dir\\""#
        );
    }

    #[test]
    fn migration_makes_one_default_tool_without_rules() {
        let config = DiffToolConfig::from(LegacyDiffToolConfig::default_tortoise());
        assert_eq!(config.tools.len(), 1);
        assert!(config.rules.is_empty());
        assert_eq!(config.tool(&config.default_tool), config.tools.first());
    }

    #[test]
    fn rule_matches_the_kind_of_the_extension() {
        let config = config(vec![rule(DiffToolMatch::Kind(FileKind::Image), "first")]);
        let mut kinds = KindTable::default();
        assert!(kinds.set_override("dds", FileKind::Image));
        assert_eq!(
            chosen(&config, &kinds, "sub/a.PNG").as_deref(),
            Some("first")
        );
        assert_eq!(
            chosen(&config, &kinds, "sub/a.dds").as_deref(),
            Some("first")
        );
        assert_eq!(
            chosen(&config, &kinds, "sub/a.txt").as_deref(),
            Some("default")
        );
    }

    #[test]
    fn rule_matches_the_extension_normalized() {
        let config = config(vec![rule(
            DiffToolMatch::Extension(".UAsset".to_string()),
            "first",
        )]);
        let kinds = KindTable::default();
        assert_eq!(
            chosen(&config, &kinds, "Content/a.uasset").as_deref(),
            Some("first")
        );
        assert_eq!(
            chosen(&config, &kinds, "Content/a.umap").as_deref(),
            Some("default")
        );
    }

    #[test]
    fn first_matching_rule_wins() {
        let config = config(vec![
            rule(DiffToolMatch::Extension("png".to_string()), "first"),
            rule(DiffToolMatch::Kind(FileKind::Image), "second"),
        ]);
        let kinds = KindTable::default();
        assert_eq!(chosen(&config, &kinds, "a.png").as_deref(), Some("first"));
        assert_eq!(chosen(&config, &kinds, "a.jpg").as_deref(), Some("second"));
    }

    #[test]
    fn unmatched_files_fall_back_to_the_default_tool() {
        let config = config(vec![rule(DiffToolMatch::Kind(FileKind::Audio), "first")]);
        let kinds = KindTable::default();
        assert_eq!(chosen(&config, &kinds, "a.txt").as_deref(), Some("default"));
        assert_eq!(
            chosen(&config, &kinds, "Makefile").as_deref(),
            Some("default")
        );
    }

    #[test]
    fn rule_naming_a_missing_tool_is_skipped() {
        let config = config(vec![
            rule(DiffToolMatch::Kind(FileKind::Image), "gone"),
            rule(DiffToolMatch::Kind(FileKind::Image), "second"),
        ]);
        let kinds = KindTable::default();
        assert_eq!(chosen(&config, &kinds, "a.png").as_deref(), Some("second"));
    }

    #[test]
    fn missing_default_tool_chooses_nothing() {
        let config = DiffToolConfig {
            default_tool: "gone".to_string(),
            ..config(Vec::new())
        };
        assert_eq!(chosen(&config, &KindTable::default(), "a.txt"), None);
    }

    #[cfg(windows)]
    #[test]
    fn presets_cover_the_common_diff_tools() {
        let names: Vec<_> = presets(None).iter().map(|preset| preset.name).collect();
        assert_eq!(
            names,
            [
                "zdiff-gui",
                "TortoiseMerge",
                "WinMerge",
                "Beyond Compare",
                "p4merge",
                "VS Code",
                "Meld"
            ]
        );
    }

    #[test]
    fn every_preset_template_validates_and_expands_for_a_sample_pair() {
        let values = pair(r"C:\My Files\a b.txt", r"D:\Other Dir\a b.txt");
        for preset in presets(Some(Path::new(r"C:\Users\me\AppData\Local"))) {
            let expanded = expand_template(preset.template, &values)
                .unwrap_or_else(|err| panic!("{}: {err}", preset.name));
            assert!(
                expanded.contains(r#""C:\My Files\a b.txt""#)
                    && expanded.contains(r#""D:\Other Dir\a b.txt""#),
                "{}: {expanded}",
                preset.name
            );
            assert!(preview_command(&preset.to_tool(|_| false)).is_ok());
        }
    }

    #[cfg(windows)]
    #[test]
    fn preset_uses_the_first_install_path_that_exists() {
        let all = presets(Some(Path::new(r"C:\Users\me\AppData\Local")));
        let vs_code = all.iter().find(|preset| preset.name == "VS Code").unwrap();
        let tool = vs_code.to_tool(|path| path.starts_with(r"C:\Program Files"));
        assert_eq!(
            tool.exe_path,
            Path::new(r"C:\Program Files\Microsoft VS Code\Code.exe")
        );
        let tool = vs_code.to_tool(|_| true);
        assert_eq!(
            tool.exe_path,
            Path::new(r"C:\Users\me\AppData\Local\Programs\Microsoft VS Code\Code.exe")
        );
        assert_eq!(tool.name, "VS Code");
        assert_eq!(tool.template, vs_code.template);
        assert_eq!(tool.kinds, vs_code.kinds);
    }

    #[cfg(windows)]
    #[test]
    fn preset_not_installed_at_its_usual_path_uses_its_bare_name_for_the_path_lookup() {
        let all = presets(None);
        let winmerge = all.iter().find(|preset| preset.name == "WinMerge").unwrap();
        assert!(!winmerge.install_paths.is_empty());
        let tool = winmerge.to_tool(|_| false);
        assert_eq!(tool.exe_path, Path::new("WinMergeU.exe"));
        let resolved = resolve_exe(
            &tool.exe_path,
            None,
            None,
            |_| false,
            |name| Some(Path::new(r"D:\Tools").join(name)),
        );
        assert_eq!(resolved.unwrap(), Path::new(r"D:\Tools\WinMergeU.exe"));
    }

    #[test]
    fn default_config_is_the_zdiff_gui_preset() {
        let config = DiffToolConfig::default();
        let zdiff = presets(None).remove(0).to_tool(|_| false);
        assert_eq!(config, DiffToolConfig::from_tool(zdiff));
        assert_eq!(
            config.tool(&config.default_tool).unwrap().exe_path,
            Path::new("zdiff-gui.exe")
        );
    }

    #[test]
    fn preview_shows_the_exe_and_the_expanded_template() {
        let tool = DiffTool {
            exe_path: PathBuf::from(r"C:\Tools\diff.exe"),
            template: r#""{left}" "{right}" -nl "{left_name}""#.to_string(),
            ..tool("diff")
        };
        assert_eq!(
            preview_command(&tool).unwrap(),
            r#""C:\Tools\diff.exe" "C:\Temp\Left Folder\sample.txt" "C:\Temp\Right Folder\sample.txt" -nl "Folder 1: sub/sample.txt""#
        );
    }

    #[test]
    fn preview_reports_a_broken_template() {
        let broken = |template: &str| DiffTool {
            template: template.to_string(),
            ..tool("broken")
        };
        assert_eq!(
            preview_command(&broken("{left}")),
            Err(TemplateError::MissingRight)
        );
        assert_eq!(
            preview_command(&broken("{left} {rihgt}")),
            Err(TemplateError::UnknownPlaceholder("rihgt".to_string()))
        );
    }

    #[test]
    fn adding_a_tool_with_a_taken_name_numbers_it() {
        let mut config = config(Vec::new());
        assert_eq!(config.add_tool(tool("WinMerge")), "WinMerge");
        assert_eq!(config.add_tool(tool("WinMerge")), "WinMerge 2");
        assert_eq!(config.add_tool(tool("WinMerge")), "WinMerge 3");
        let names: Vec<_> = config.tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "default",
                "first",
                "second",
                "WinMerge",
                "WinMerge 2",
                "WinMerge 3"
            ]
        );
    }

    #[test]
    fn renaming_a_tool_updates_its_rules_and_the_default() {
        let mut config = config(vec![
            rule(DiffToolMatch::Kind(FileKind::Image), "default"),
            rule(DiffToolMatch::Kind(FileKind::Text), "first"),
        ]);
        config.rename_tool("default", "renamed").unwrap();
        assert_eq!(config.default_tool, "renamed");
        assert_eq!(config.tools[0].name, "renamed");
        assert_eq!(config.rules[0].tool, "renamed");
        assert_eq!(config.rules[1].tool, "first");
    }

    #[test]
    fn renaming_to_an_empty_or_taken_name_is_rejected() {
        let mut config = config(Vec::new());
        assert_eq!(config.rename_tool("first", " "), Err(ToolNameError::Empty));
        assert_eq!(
            config.rename_tool("first", "second"),
            Err(ToolNameError::Taken("second".to_string()))
        );
        assert_eq!(config.rename_tool("first", "first"), Ok(()));
        assert_eq!(config.tools[1].name, "first");
    }

    #[test]
    fn removing_a_tool_drops_its_rules() {
        let mut config = config(vec![
            rule(DiffToolMatch::Kind(FileKind::Image), "first"),
            rule(DiffToolMatch::Kind(FileKind::Text), "second"),
        ]);
        config.remove_tool("first");
        assert!(config.tool("first").is_none());
        assert_eq!(
            config.rules,
            [rule(DiffToolMatch::Kind(FileKind::Text), "second")]
        );
        assert_eq!(config.default_tool, "default");
    }

    #[test]
    fn removing_the_default_tool_passes_the_default_to_the_first_remaining_tool() {
        let mut config = config(Vec::new());
        config.remove_tool("default");
        assert_eq!(config.default_tool, "first");
        assert!(
            config
                .choose_tool(&KindTable::default(), Path::new("a.txt"))
                .is_some()
        );
    }

    mod exe_resolution {
        use super::*;

        const APP_DIR: &str = r"C:\apps\zdiff-folder";
        const BUILD_ZDIFF: &str = r"E:\repo\target\debug\zdiff-gui.exe";
        const ON_PATH_DIR: &str = r"C:\tools";

        fn resolve(exe: &str, files: &[&str], path_files: &[&str]) -> Result<PathBuf, ExeNotFound> {
            resolve_exe(
                Path::new(exe),
                Some(Path::new(APP_DIR)),
                Some(Path::new(BUILD_ZDIFF)),
                |path| files.iter().any(|file| Path::new(file) == path),
                |name| {
                    path_files
                        .iter()
                        .any(|file| Path::new(file) == name)
                        .then(|| Path::new(ON_PATH_DIR).join(name))
                },
            )
        }

        #[test]
        fn bare_name_resolves_next_to_the_app_first() {
            let next_to_app = r"C:\apps\zdiff-folder\zdiff-gui.exe";
            assert_eq!(
                resolve(
                    "zdiff-gui.exe",
                    &[next_to_app, BUILD_ZDIFF],
                    &["zdiff-gui.exe"]
                ),
                Ok(PathBuf::from(next_to_app))
            );
        }

        #[test]
        fn bare_zdiff_name_resolves_at_the_build_time_path() {
            assert_eq!(
                resolve("ZDiff-GUI.exe", &[BUILD_ZDIFF], &["ZDiff-GUI.exe"]),
                Ok(PathBuf::from(BUILD_ZDIFF))
            );
        }

        #[test]
        fn bare_name_resolves_on_path_last() {
            assert_eq!(
                resolve("zdiff-gui.exe", &[], &["zdiff-gui.exe"]),
                Ok(PathBuf::from(r"C:\tools\zdiff-gui.exe"))
            );
        }

        #[test]
        fn other_bare_names_skip_the_build_time_path() {
            let resolve_other = |path_files: &[&str]| {
                resolve_exe(
                    Path::new("TortoiseProc.exe"),
                    Some(Path::new(APP_DIR)),
                    Some(Path::new(BUILD_ZDIFF)),
                    |_| false,
                    |name| {
                        path_files
                            .contains(&name.to_str().unwrap())
                            .then(|| Path::new(ON_PATH_DIR).join(name))
                    },
                )
            };
            assert_eq!(
                resolve_other(&["TortoiseProc.exe"]),
                Ok(PathBuf::from(r"C:\tools\TortoiseProc.exe"))
            );
            assert_eq!(
                resolve_other(&[]),
                Err(ExeNotFound {
                    exe: PathBuf::from("TortoiseProc.exe"),
                    searched: vec![
                        r"C:\apps\zdiff-folder\TortoiseProc.exe".to_string(),
                        "PATH".to_string(),
                    ],
                })
            );
        }

        #[test]
        fn path_with_folders_is_used_verbatim() {
            for exe in [
                r"C:\Program Files\TortoiseSVN\bin\TortoiseProc.exe",
                r"bin\zdiff-gui.exe",
            ] {
                assert_eq!(resolve(exe, &[], &[]), Ok(PathBuf::from(exe)));
            }
        }

        #[test]
        fn nothing_found_lists_the_places_searched() {
            let err = resolve("zdiff-gui.exe", &[], &[]).unwrap_err();
            assert_eq!(
                err.searched,
                [r"C:\apps\zdiff-folder\zdiff-gui.exe", BUILD_ZDIFF, "PATH"]
            );
            assert_eq!(
                err.to_string(),
                r"zdiff-gui.exe not found, searched: C:\apps\zdiff-folder\zdiff-gui.exe, E:\repo\target\debug\zdiff-gui.exe, PATH"
            );
        }

        #[test]
        fn missing_app_dir_and_build_path_go_straight_to_path() {
            let err = resolve_exe(Path::new("zdiff-gui.exe"), None, None, |_| true, |_| None)
                .unwrap_err();
            assert_eq!(err.searched, ["PATH"]);
        }
    }

    #[cfg(feature = "serde")]
    #[test]
    fn persisted_legacy_config_loads_as_one_migrated_tool() {
        let json = r#"{
            "exe_path": "C:\\Program Files\\TortoiseSVN\\bin\\TortoiseProc.exe",
            "prefix_args": { "default_args": ["/command:diff"] },
            "diff_path_1_args": "/path:\"{}\"",
            "diff_path_2_args": "/path2:\"{}\"",
            "suffix_args": { "default_args": ["/closeonend:1"] }
        }"#;
        let config: DiffToolConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            config,
            DiffToolConfig::from(LegacyDiffToolConfig::default_tortoise())
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn persisted_config_round_trips() {
        let config = DiffToolConfig {
            tools: vec![DiffTool {
                kinds: vec![FileKind::Text, FileKind::UnrealAsset],
                ..tool("first")
            }],
            ..config(vec![
                rule(DiffToolMatch::Kind(FileKind::Image), "first"),
                rule(DiffToolMatch::Extension("umap".to_string()), "second"),
            ])
        };
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(
            serde_json::from_str::<DiffToolConfig>(&json).unwrap(),
            config
        );
    }

    fn kinds_tool(kinds: &[FileKind]) -> DiffTool {
        DiffTool {
            kinds: kinds.to_vec(),
            ..tool("tool")
        }
    }

    #[test]
    fn same_kinds_the_tool_lists_can_be_diffed() {
        let tool = kinds_tool(&[FileKind::Text, FileKind::Image]);
        let image = Some(FileKind::Image);
        assert_eq!(cant_diff([image, image], &tool), None);
    }

    #[test]
    fn same_kinds_the_tool_does_not_list_name_the_tool_and_kind() {
        let tool = kinds_tool(&[FileKind::Text]);
        let asset = Some(FileKind::UnrealAsset);
        let reason = cant_diff([asset, asset], &tool).unwrap();
        assert_eq!(
            reason,
            CantDiff {
                tool: "tool".to_string(),
                kinds: [FileKind::UnrealAsset, FileKind::UnrealAsset],
            }
        );
        assert_eq!(reason.to_string(), "tool can't diff UnrealAsset files");
    }

    #[test]
    fn mixed_kinds_can_be_diffed_only_if_the_tool_lists_both() {
        let pair = [Some(FileKind::Text), Some(FileKind::Image)];
        assert_eq!(
            cant_diff(pair, &kinds_tool(&[FileKind::Image, FileKind::Text])),
            None
        );
        for listed in [&[FileKind::Text][..], &[FileKind::Image], &[]] {
            let reason = cant_diff(pair, &kinds_tool(listed)).unwrap();
            assert_eq!(
                reason.kinds,
                [FileKind::Text, FileKind::Image],
                "{listed:?}"
            );
            assert_eq!(reason.to_string(), "tool can't diff Text against Image");
        }
    }

    #[test]
    fn one_sided_rows_and_folders_are_never_marked() {
        let tool = kinds_tool(&[]);
        let text = Some(FileKind::Text);
        assert_eq!(cant_diff([text, None], &tool), None);
        assert_eq!(cant_diff([None, text], &tool), None);
        assert_eq!(cant_diff([None, None], &tool), None);
    }

    #[test]
    fn zdiff_gui_preset_lists_the_kinds_zdiff_gui_has_a_viewer_for() {
        let zdiff = presets(None).remove(0);
        assert_eq!(zdiff.name, "zdiff-gui");
        assert_eq!(
            zdiff.kinds,
            [FileKind::Text, FileKind::Image, FileKind::DebugSymbols]
        );
        let config = DiffToolConfig::default();
        assert_eq!(
            config.tool(&config.default_tool).unwrap().kinds,
            [FileKind::Text, FileKind::Image, FileKind::DebugSymbols]
        );
    }

    #[test]
    fn marking_undiffable_rows_defaults_to_off() {
        assert!(!DiffToolConfig::default().mark_undiffable);
        assert!(!DiffToolConfig::from(LegacyDiffToolConfig::default_zdiff()).mark_undiffable);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn marking_undiffable_rows_persists_and_is_off_in_configs_saved_without_it() {
        let config = DiffToolConfig {
            mark_undiffable: true,
            ..config(Vec::new())
        };
        let json = serde_json::to_string(&config).unwrap();
        let loaded = serde_json::from_str::<DiffToolConfig>(&json).unwrap();
        assert!(loaded.mark_undiffable);

        let json = r#"{ "tools": [], "rules": [], "default_tool": "default" }"#;
        let loaded = serde_json::from_str::<DiffToolConfig>(json).unwrap();
        assert!(!loaded.mark_undiffable);
    }
}
