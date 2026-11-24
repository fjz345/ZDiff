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

impl fmt::Display for DiffToolDefaultArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.default_args.join("\n"))
    }
}

impl DiffToolDefaultArgs {
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
            exe_path: PathBuf::from(BUNDLED_EXE_NAME),
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

const BUNDLED_EXE_NAME: &str = if cfg!(windows) {
    WINDOWS_BUNDLED_EXE_NAME
} else {
    "zdiff-gui"
};
const WINDOWS_BUNDLED_EXE_NAME: &str = "zdiff-gui.exe";

impl DiffTool {
    pub fn is_bundled(&self) -> bool {
        self.exe_path.to_str().is_some_and(|exe| {
            [BUNDLED_EXE_NAME, WINDOWS_BUNDLED_EXE_NAME]
                .iter()
                .any(|name| exe.eq_ignore_ascii_case(name))
        })
    }

    pub fn exe_to_launch(&self) -> &Path {
        if self.is_bundled() {
            Path::new(BUNDLED_EXE_NAME)
        } else {
            &self.exe_path
        }
    }
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

fn zdiff_gui_preset() -> DiffToolPreset {
    DiffToolPreset {
        name: "zdiff-gui",
        install_paths: Vec::new(),
        bare_name: BUNDLED_EXE_NAME,
        template: r#""{left}" "{right}" -nl "{left_name}" -nr "{right_name}""#,
        kinds: FileKind::ALL
            .into_iter()
            .filter(|&kind| zdiff_gui_handles(kind))
            .collect(),
    }
}

pub fn presets(local_app_data: Option<&Path>) -> Vec<DiffToolPreset> {
    if cfg!(windows) {
        windows_presets(local_app_data)
    } else {
        unix_presets(cfg!(target_os = "macos"))
    }
}

pub(crate) fn windows_presets(local_app_data: Option<&Path>) -> Vec<DiffToolPreset> {
    let paths = |paths: &[&str]| paths.iter().map(PathBuf::from).collect::<Vec<_>>();
    let vs_code_user = local_app_data
        .map(|dir| dir.join(r"Programs\Microsoft VS Code\Code.exe"))
        .into_iter();
    vec![
        zdiff_gui_preset(),
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

// Apps started from the macOS Finder get a PATH without /usr/local/bin, hence the install paths.
fn unix_presets(macos: bool) -> Vec<DiffToolPreset> {
    let macos_paths = |paths: &[&str]| match macos {
        true => paths.iter().map(PathBuf::from).collect(),
        false => Vec::new(),
    };
    let mut presets = vec![
        zdiff_gui_preset(),
        DiffToolPreset {
            name: "Meld",
            install_paths: Vec::new(),
            bare_name: "meld",
            template: r#"--label "{left_name}" --label "{right_name}" "{left}" "{right}""#,
            kinds: vec![FileKind::Text],
        },
        DiffToolPreset {
            name: "KDiff3",
            install_paths: Vec::new(),
            bare_name: "kdiff3",
            template: r#"--L1 "{left_name}" --L2 "{right_name}" "{left}" "{right}""#,
            kinds: vec![FileKind::Text],
        },
        DiffToolPreset {
            name: "VS Code",
            install_paths: macos_paths(&[
                "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
            ]),
            bare_name: "code",
            template: r#"--diff "{left}" "{right}""#,
            kinds: vec![FileKind::Text],
        },
        DiffToolPreset {
            name: "Beyond Compare",
            install_paths: macos_paths(&["/usr/local/bin/bcompare"]),
            bare_name: "bcompare",
            template: r#""{left}" "{right}" -lefttitle="{left_name}" -righttitle="{right_name}""#,
            kinds: vec![FileKind::Text, FileKind::Image],
        },
        DiffToolPreset {
            name: "p4merge",
            install_paths: macos_paths(&["/Applications/p4merge.app/Contents/MacOS/p4merge"]),
            bare_name: "p4merge",
            template: r#"-nl "{left_name}" -nr "{right_name}" "{left}" "{right}""#,
            kinds: vec![FileKind::Text],
        },
    ];
    if macos {
        presets.push(DiffToolPreset {
            name: "FileMerge",
            install_paths: Vec::new(),
            bare_name: "opendiff",
            template: r#""{left}" "{right}""#,
            kinds: vec![FileKind::Text],
        });
    }
    presets
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
    UnclosedQuote,
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateError::UnknownPlaceholder(name) => write!(f, "unknown placeholder {{{name}}}"),
            TemplateError::UnclosedPlaceholder => write!(f, "'{{' without a closing '}}'"),
            TemplateError::MissingLeft => write!(f, "missing {{left}}"),
            TemplateError::MissingRight => write!(f, "missing {{right}}"),
            TemplateError::QuoteInValue(value) => write!(f, "{value:?} contains a double quote"),
            TemplateError::UnclosedQuote => write!(f, "'\"' without a closing '\"'"),
        }
    }
}

impl std::error::Error for TemplateError {}

pub fn expand_template(template: &str, pair: &DiffPair) -> Result<String, TemplateError> {
    let mut found = [false; 2];
    let expanded = substitute(template, pair, &mut found, true)?;
    require_both_sides(found)?;
    Ok(expanded)
}

/// Splits before substituting, so values may hold quotes and spaces.
pub fn expand_template_args(template: &str, pair: &DiffPair) -> Result<Vec<String>, TemplateError> {
    let mut found = [false; 2];
    let args = split_args(template)?
        .iter()
        .map(|arg| substitute(arg, pair, &mut found, false))
        .collect::<Result<Vec<_>, _>>()?;
    require_both_sides(found)?;
    Ok(args)
}

fn require_both_sides([has_left, has_right]: [bool; 2]) -> Result<(), TemplateError> {
    if !has_left {
        return Err(TemplateError::MissingLeft);
    }
    if !has_right {
        return Err(TemplateError::MissingRight);
    }
    Ok(())
}

/// Single pass, so placeholder text inside a value (a file named `{right}.txt`) stays literal.
fn substitute(
    template: &str,
    pair: &DiffPair,
    [has_left, has_right]: &mut [bool; 2],
    reject_quotes: bool,
) -> Result<String, TemplateError> {
    let mut expanded = String::with_capacity(template.len() + pair.left.len() + pair.right.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        expanded.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let close = after_open
            .find('}')
            .ok_or(TemplateError::UnclosedPlaceholder)?;
        let value = match &after_open[..close] {
            "left" => {
                *has_left = true;
                pair.left
            }
            "right" => {
                *has_right = true;
                pair.right
            }
            "left_name" => pair.left_name,
            "right_name" => pair.right_name,
            unknown => return Err(TemplateError::UnknownPlaceholder(unknown.to_string())),
        };
        // Values go inside the template's quotes, so a quote would end them and start new arguments.
        if reject_quotes && value.contains('"') {
            return Err(TemplateError::QuoteInValue(value.to_string()));
        }
        expanded.push_str(value);
        rest = &after_open[close + 1..];
    }
    expanded.push_str(rest);
    Ok(expanded)
}

pub const SAMPLE_PAIR: DiffPair<'static> = DiffPair {
    left: if cfg!(windows) {
        r"C:\Temp\Left Folder\sample.txt"
    } else {
        "/tmp/Left Folder/sample.txt"
    },
    right: if cfg!(windows) {
        r"C:\Temp\Right Folder\sample.txt"
    } else {
        "/tmp/Right Folder/sample.txt"
    },
    left_name: "Folder 1: sub/sample.txt",
    right_name: "Folder 2: sub/sample.txt",
};

pub fn preview_command(tool: &DiffTool) -> Result<String, TemplateError> {
    if cfg!(windows) {
        let args = expand_template(&tool.template, &SAMPLE_PAIR)?;
        return Ok(format!("\"{}\" {args}", tool.exe_path.display()));
    }
    let args = expand_template_args(&tool.template, &SAMPLE_PAIR)?;
    let quoted = |arg: String| {
        if arg.contains(char::is_whitespace) {
            format!("\"{arg}\"")
        } else {
            arg
        }
    };
    Ok(std::iter::once(tool.exe_path.display().to_string())
        .chain(args)
        .map(quoted)
        .collect::<Vec<_>>()
        .join(" "))
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

    let mut searched = Vec::new();
    if let Some(next_to_app) = app_dir.map(|dir| dir.join(exe)) {
        if is_file(&next_to_app) {
            return Ok(next_to_app);
        }
        searched.push(next_to_app.display().to_string());
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

fn split_args(args: &str) -> Result<Vec<String>, TemplateError> {
    let mut split = Vec::new();
    let mut current: Option<String> = None;
    let mut quoted = false;
    for c in args.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                current.get_or_insert_with(String::new);
            }
            c if c.is_whitespace() && !quoted => split.extend(current.take()),
            c => current.get_or_insert_with(String::new).push(c),
        }
    }
    if quoted {
        return Err(TemplateError::UnclosedQuote);
    }
    split.extend(current);
    Ok(split)
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

#[cfg(not(target_os = "windows"))]
fn open_diff_tool_unix(exe: &Path, tool: &DiffTool, pair: &DiffPair) -> io::Result<Child> {
    let args = expand_template_args(&tool.template, pair)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let mut cmd = Command::new(exe);
    cmd.args(args);

    log::info!("Opening diff tool {:?}", cmd);
    cmd.spawn()
}

// Unix keeps an exited child as a zombie until someone waits on it.
pub fn wait_in_background(mut child: Child) {
    std::thread::spawn(move || {
        if let Err(err) = child.wait() {
            log::warn!("waiting for a launched process: {err}");
        }
    });
}

pub fn open_diff_tool(exe: &Path, tool: &DiffTool, pair: &DiffPair) -> io::Result<Child> {
    #[cfg(target_os = "windows")]
    return open_diff_tool_windows(exe, tool, pair);

    #[cfg(not(target_os = "windows"))]
    open_diff_tool_unix(exe, tool, pair)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zcommon::test_util::os_path;

    const LEFT: &str = r"C:\left\sub\a.txt";
    const RIGHT: &str = r"D:\right\sub\a.txt";
    const ZDIFF_GUI_EXE: &str = if cfg!(windows) {
        "zdiff-gui.exe"
    } else {
        "zdiff-gui"
    };

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
    fn the_bare_zdiff_gui_tool_is_bundled_and_a_full_path_or_another_tool_is_not() {
        let tool = |exe_path: &str| DiffTool {
            exe_path: exe_path.into(),
            ..DiffToolConfig::default().tools.remove(0)
        };
        assert!(DiffToolConfig::default().tools[0].is_bundled());
        assert!(tool("ZDiff-GUI.exe").is_bundled());
        assert!(DiffTool::from(LegacyDiffToolConfig::default_zdiff()).is_bundled());
        assert!(!tool(&os_path(r"C:\apps\zdiff-gui.exe")).is_bundled());
        assert!(!tool(&os_path(r"bin\zdiff-gui.exe")).is_bundled());
        assert!(!tool(&os_path(r"C:\apps\zdiff-gui")).is_bundled());
        assert_eq!(tool("zdiff-gui").is_bundled(), !cfg!(windows));
        assert!(!tool("TortoiseMerge.exe").is_bundled());
    }

    #[test]
    fn migrated_zdiff_config_expands_to_todays_command_line() {
        let tool = DiffTool::from(LegacyDiffToolConfig::default_zdiff());
        assert_eq!(tool.exe_path, PathBuf::from(ZDIFF_GUI_EXE));
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
    fn unix_presets_cover_the_common_diff_tools_and_filemerge_on_macos() {
        let names = |macos| {
            unix_presets(macos)
                .iter()
                .map(|preset| preset.name)
                .collect::<Vec<_>>()
        };
        let linux = [
            "zdiff-gui",
            "Meld",
            "KDiff3",
            "VS Code",
            "Beyond Compare",
            "p4merge",
        ];
        assert_eq!(names(false), linux);
        assert_eq!(names(true), [&linux[..], &["FileMerge"]].concat());
        assert!(
            unix_presets(false)
                .iter()
                .all(|preset| preset.install_paths.is_empty())
        );
        assert_eq!(unix_presets(true)[0].bare_name, ZDIFF_GUI_EXE);
    }

    #[test]
    fn unix_preset_args_hold_the_paths_and_labels_whole() {
        let values = DiffPair {
            left: "/home/me/My Files/a b.txt",
            right: "/mnt/other dir/a b.txt",
            left_name: "Folder 1: sub/a b.txt",
            right_name: "Folder 2: sub/a b.txt",
        };
        for preset in unix_presets(true) {
            let args = expand_template_args(preset.template, &values).unwrap();
            let has_arg = |value: &str| args.iter().any(|arg| arg == value);
            assert!(
                has_arg(values.left) && has_arg(values.right),
                "{}: {args:?}",
                preset.name
            );
            for (label, placeholder) in [
                (values.left_name, "{left_name}"),
                (values.right_name, "{right_name}"),
            ] {
                assert_eq!(
                    args.iter().any(|arg| arg.ends_with(label)),
                    preset.template.contains(placeholder),
                    "{}: {args:?}",
                    preset.name
                );
            }
        }
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
            Path::new(ZDIFF_GUI_EXE)
        );
    }

    #[test]
    fn preview_shows_the_exe_and_the_expanded_template() {
        let tool = DiffTool {
            exe_path: PathBuf::from(os_path(r"C:\Tools\diff.exe")),
            template: r#""{left}" "{right}" -nl "{left_name}""#.to_string(),
            ..tool("diff")
        };
        let expected = if cfg!(windows) {
            r#""C:\Tools\diff.exe" "C:\Temp\Left Folder\sample.txt" "C:\Temp\Right Folder\sample.txt" -nl "Folder 1: sub/sample.txt""#
        } else {
            r#"/Tools/diff.exe "/tmp/Left Folder/sample.txt" "/tmp/Right Folder/sample.txt" -nl "Folder 1: sub/sample.txt""#
        };
        assert_eq!(preview_command(&tool).unwrap(), expected);
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

    #[test]
    fn split_args_groups_quoted_text_and_splits_on_whitespace() {
        assert_eq!(
            split_args("\"/home/me/My Files/a.txt\"   -nl \"Folder 1: sub/a b.txt\"\tplain ")
                .unwrap(),
            [
                "/home/me/My Files/a.txt",
                "-nl",
                "Folder 1: sub/a b.txt",
                "plain"
            ]
        );
    }

    #[test]
    fn split_args_keeps_an_empty_quoted_arg_and_drops_blank_text() {
        assert_eq!(split_args(r#"-a "" -b"#).unwrap(), ["-a", "", "-b"]);
        assert_eq!(split_args(" \t ").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn split_args_joins_quoted_and_bare_text_of_one_arg() {
        assert_eq!(
            split_args(r#"-lefttitle="Folder 1: a.txt" x"y z"w"#).unwrap(),
            ["-lefttitle=Folder 1: a.txt", "xy zw"]
        );
    }

    #[test]
    fn split_args_keeps_backslashes_literal() {
        assert_eq!(split_args(r#""a\b" c\"#).unwrap(), [r"a\b", r"c\"]);
    }

    #[test]
    fn template_args_keep_values_whole_even_with_quotes_and_spaces() {
        let values = DiffPair {
            left: r#"/a "b" c.txt"#,
            right: "/d e.txt",
            left_name: r#"Folder 1: x "y""#,
            right_name: "Folder 2: z",
        };
        assert_eq!(
            expand_template_args(
                r#"{left} "{right}" -lefttitle="{left_name}" --label {right_name}"#,
                &values
            )
            .unwrap(),
            [
                r#"/a "b" c.txt"#,
                "/d e.txt",
                r#"-lefttitle=Folder 1: x "y""#,
                "--label",
                "Folder 2: z"
            ]
        );
    }

    #[test]
    fn template_args_report_template_errors() {
        let values = pair(LEFT, RIGHT);
        assert_eq!(
            expand_template_args("{left}", &values),
            Err(TemplateError::MissingRight)
        );
        assert_eq!(
            expand_template_args("{left} {rihgt}", &values),
            Err(TemplateError::UnknownPlaceholder("rihgt".to_string()))
        );
        assert_eq!(
            expand_template_args(r#""{left} {right}"#, &values),
            Err(TemplateError::UnclosedQuote)
        );
    }

    #[test]
    fn a_bundled_tool_launches_this_platforms_zdiff_gui() {
        let with_exe = |exe: &str| DiffTool {
            exe_path: PathBuf::from(exe),
            ..tool("t")
        };
        for saved in [ZDIFF_GUI_EXE, "ZDiff-GUI.exe"] {
            assert_eq!(with_exe(saved).exe_to_launch(), Path::new(ZDIFF_GUI_EXE));
        }
        let full_path = os_path(r"C:\apps\zdiff-gui.exe");
        assert_eq!(with_exe(&full_path).exe_to_launch(), Path::new(&full_path));
    }

    #[test]
    fn split_args_rejects_an_unclosed_quote() {
        assert_eq!(
            split_args(r#""/a b.txt" "/c d.txt"#),
            Err(TemplateError::UnclosedQuote)
        );
    }

    mod exe_resolution {
        use super::*;

        const APP_DIR: &str = r"C:\apps\zdiff-folder";
        const ON_PATH_DIR: &str = r"C:\tools";

        fn resolve(exe: &str, files: &[&str], path_files: &[&str]) -> Result<PathBuf, ExeNotFound> {
            resolve_exe(
                Path::new(exe),
                Some(Path::new(&os_path(APP_DIR))),
                |path| files.iter().any(|file| Path::new(file) == path),
                |name| {
                    path_files
                        .iter()
                        .any(|file| Path::new(file) == name)
                        .then(|| Path::new(&os_path(ON_PATH_DIR)).join(name))
                },
            )
        }

        #[test]
        fn bare_name_resolves_next_to_the_app_first() {
            let next_to_app = os_path(r"C:\apps\zdiff-folder\zdiff-gui.exe");
            assert_eq!(
                resolve("zdiff-gui.exe", &[next_to_app.as_str()], &["zdiff-gui.exe"]),
                Ok(PathBuf::from(next_to_app))
            );
        }

        #[test]
        fn bare_name_resolves_on_path_last() {
            assert_eq!(
                resolve("zdiff-gui.exe", &[], &["zdiff-gui.exe"]),
                Ok(PathBuf::from(os_path(r"C:\tools\zdiff-gui.exe")))
            );
        }

        #[test]
        fn any_bare_name_is_looked_up_next_to_the_app_then_on_path() {
            let resolve_other = |path_files: &[&str]| {
                resolve_exe(
                    Path::new("TortoiseProc.exe"),
                    Some(Path::new(&os_path(APP_DIR))),
                    |_| false,
                    |name| {
                        path_files
                            .contains(&name.to_str().unwrap())
                            .then(|| Path::new(&os_path(ON_PATH_DIR)).join(name))
                    },
                )
            };
            assert_eq!(
                resolve_other(&["TortoiseProc.exe"]),
                Ok(PathBuf::from(os_path(r"C:\tools\TortoiseProc.exe")))
            );
            assert_eq!(
                resolve_other(&[]),
                Err(ExeNotFound {
                    exe: PathBuf::from("TortoiseProc.exe"),
                    searched: vec![
                        os_path(r"C:\apps\zdiff-folder\TortoiseProc.exe"),
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
            ]
            .map(os_path)
            {
                assert_eq!(resolve(&exe, &[], &[]), Ok(PathBuf::from(exe)));
            }
        }

        #[test]
        fn nothing_found_lists_the_places_searched() {
            let next_to_app = os_path(r"C:\apps\zdiff-folder\zdiff-gui.exe");
            let err = resolve("zdiff-gui.exe", &[], &[]).unwrap_err();
            assert_eq!(err.searched, [next_to_app.clone(), "PATH".to_string()]);
            assert_eq!(
                err.to_string(),
                format!("zdiff-gui.exe not found, searched: {next_to_app}, PATH")
            );
        }

        #[test]
        fn missing_app_dir_goes_straight_to_path() {
            let err =
                resolve_exe(Path::new("zdiff-gui.exe"), None, |_| true, |_| None).unwrap_err();
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
