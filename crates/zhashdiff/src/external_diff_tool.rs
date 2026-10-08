use std::fmt;
use std::io;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
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
}

#[cfg(feature = "serde")]
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum PersistedDiffToolConfig {
    Current {
        tools: Vec<DiffTool>,
        rules: Vec<DiffToolRule>,
        default_tool: String,
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
            } => Self {
                tools,
                rules,
                default_tool,
            },
            PersistedDiffToolConfig::Legacy(legacy) => legacy.into(),
        }
    }
}

impl Default for DiffToolConfig {
    fn default() -> Self {
        LegacyDiffToolConfig::default_zdiff().into()
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
        }
    }

    pub fn tool(&self, name: &str) -> Option<&DiffTool> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    pub fn tool_mut(&mut self, name: &str) -> Option<&mut DiffTool> {
        self.tools.iter_mut().find(|tool| tool.name == name)
    }

    /// First rule whose tool exists wins, else the default tool; `None` if that is missing too.
    pub fn choose_tool(&self, kinds: &KindTable, path: &Path) -> Option<&DiffTool> {
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
                if tool.is_none() {
                    log::warn!("diff tool rule {rule:?} names a missing tool, skipped");
                }
                tool
            })
            .or_else(|| self.tool(&self.default_tool))
    }
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
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateError::UnknownPlaceholder(name) => write!(f, "unknown placeholder {{{name}}}"),
            TemplateError::UnclosedPlaceholder => write!(f, "'{{' without a closing '}}'"),
            TemplateError::MissingLeft => write!(f, "missing {{left}}"),
            TemplateError::MissingRight => write!(f, "missing {{right}}"),
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

#[cfg(target_os = "windows")]
pub fn open_diff_tool_windows(tool: &DiffTool, pair: &DiffPair) -> io::Result<()> {
    let args = expand_template(&tool.template, pair)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let mut cmd = Command::new(&tool.exe_path);
    // !!! Important !!! use raw_arg.
    cmd.raw_arg(args);

    log::info!("Opening diff tool {:?}", cmd);
    cmd.spawn()?;

    Ok(())
}
#[cfg(target_os = "macos")]
pub fn open_diff_tool_macos(tool: &DiffTool, pair: &DiffPair) -> io::Result<()> {
    todo!();
    let mut cmd = Command::new(&tool.exe_path);

    log::info!("Opening diff tool {:?}", cmd);
    cmd.spawn()?;

    Ok(())
}

pub fn open_diff_tool(tool: &DiffTool, pair: &DiffPair) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    open_diff_tool_windows(tool, pair)?;

    #[cfg(target_os = "macos")]
    open_diff_tool_macos(tool, pair)?;

    Ok(())
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
}
