use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::{env, str};

use crate::universal_path::UniversalPath;

use std::sync::RwLock;

/// What every p4 command resolves its config from. Global because commands run on loader
/// threads too.
#[derive(Default)]
struct P4Settings {
    profiles: P4Profiles,
    /// The profile of the last Quick Diff; `None` is Auto.
    slot_profile: Option<u64>,
}

static GLOBAL_P4_SETTINGS: std::sync::OnceLock<RwLock<P4Settings>> = std::sync::OnceLock::new();
fn p4_settings() -> &'static RwLock<P4Settings> {
    GLOBAL_P4_SETTINGS.get_or_init(Default::default)
}
pub fn get_p4_config() -> P4Config {
    let settings = p4_settings().read().unwrap();
    // No config in the log: it holds the password.
    log::trace!(
        "Reading P4 config for slot profile {:?}",
        settings.slot_profile
    );
    settings.profiles.resolve(settings.slot_profile)
}
pub fn update_p4_profiles(profiles: P4Profiles) {
    p4_settings().write().unwrap().profiles = profiles;
}
/// Commands from now on use this slot profile over the default one.
pub fn set_p4_slot_profile(slot_profile: Option<u64>) {
    p4_settings().write().unwrap().slot_profile = slot_profile;
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct P4Config {
    pub port: String,     // e.g., "localhost:1666"
    pub user: String,     // e.g., "admin"
    pub client: String,   // e.g., "my_workspace"
    pub charset: String,  // e.g., "utf8"
    pub password: String, // e.g., "secret"
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct P4Profile {
    /// Stable across renames; Quick Diff slots and the default refer to it.
    pub id: u64,
    pub name: String,
    pub config: P4Config,
}

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct P4Profiles {
    pub profiles: Vec<P4Profile>,
    pub default: Option<u64>,
    /// Never decreases, so a slot still holding a removed id doesn't pick up a new profile.
    #[cfg_attr(feature = "serde", serde(default))]
    pub next_id: u64,
}

impl P4Profiles {
    pub fn get(&self, id: u64) -> Option<&P4Profile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    /// Adds an empty profile and returns its id, never used by a listed or removed profile.
    pub fn add(&mut self, name: String) -> u64 {
        // The listed ids too: a save without next_id loads it as 0.
        let listed_next = self.profiles.iter().map(|p| p.id + 1).max().unwrap_or(0);
        let id = self.next_id.max(listed_next);
        self.next_id = id + 1;
        self.profiles.push(P4Profile {
            id,
            name,
            config: P4Config::default(),
        });
        id
    }

    /// Slots still referring to it fall back to Auto.
    pub fn remove(&mut self, id: u64) {
        self.profiles.retain(|profile| profile.id != id);
        if self.default == Some(id) {
            self.default = None;
        }
    }

    /// The config a command runs with, field by field: the slot's profile, then the default
    /// profile, then (left empty) whatever p4 resolves from P4CONFIG or the environment. `None`
    /// is Auto, and so is a slot whose profile was removed.
    pub fn resolve(&self, slot: Option<u64>) -> P4Config {
        let empty = P4Config::default();
        let config_of = |id: Option<u64>| {
            id.and_then(|id| self.get(id))
                .map_or(&empty, |profile| &profile.config)
        };
        let (slot, default) = (config_of(slot), config_of(self.default));
        let pick = |field: fn(&P4Config) -> &String| {
            let value = field(slot);
            if value.is_empty() {
                field(default).clone()
            } else {
                value.clone()
            }
        };
        P4Config {
            port: pick(|c| &c.port),
            user: pick(|c| &c.user),
            client: pick(|c| &c.client),
            charset: pick(|c| &c.charset),
            password: pick(|c| &c.password),
        }
    }
}

/// Runs a p4 command: stdout on success, the error text on failure. A trait so logic that asks
/// p4 can be tested with a fake.
pub trait P4Runner {
    fn run(&self, args: &[&str]) -> Result<String, String>;

    fn run_bytes(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        self.run(args).map(String::into_bytes)
    }
}

impl P4Runner for P4Command {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.output(args)
    }

    fn run_bytes(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        self.output_bytes(args)
    }
}

/// A local path as a p4 file argument. p4 reads `@ # *` as revision and wildcard syntax and `%`
/// as its escape, so `p4 edit a*.txt` would open every match.
pub fn escape_local_path(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '%' => escaped.push_str("%25"),
            '@' => escaped.push_str("%40"),
            '#' => escaped.push_str("%23"),
            '*' => escaped.push_str("%2A"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// How one p4 command's environment differs from the app's.
#[derive(Debug, Default, PartialEq)]
pub struct P4Env {
    pub args: Vec<String>,
    pub set: Vec<(&'static str, String)>,
    pub unset: Vec<&'static str>,
    pub cwd: Option<PathBuf>,
}

/// A non-empty field is passed as p4's global option, `#` unsets its variable, and an empty one
/// leaves the inherited value. Options, unlike variables, win over a P4CONFIG file. p4vc (`gui`)
/// gets variables instead: its global options aren't confirmed to match p4's. A local file's
/// directory becomes the working directory so p4 finds the P4CONFIG file of the repository that
/// file lives in; a depot path has no directory to pick one, and a relative path keeps the
/// app's cwd.
pub fn resolve_p4_env(config: &P4Config, file: Option<&UniversalPath>, gui: bool) -> P4Env {
    let mut env = P4Env::default();
    for (option, var, value) in [
        ("-p", "P4PORT", &config.port),
        ("-u", "P4USER", &config.user),
        ("-P", "P4PASSWD", &config.password),
        ("-c", "P4CLIENT", &config.client),
        ("-C", "P4CHARSET", &config.charset),
    ] {
        match value.as_str() {
            "" => {}
            "#" => env.unset.push(var),
            _ if gui => env.set.push((var, value.clone())),
            _ => env.args.extend([option.to_string(), value.clone()]),
        }
    }
    env.cwd = match file {
        // A relative path would name another file once the cwd moves (it is also the argument).
        Some(UniversalPath::Local(path)) if path.is_absolute() => {
            path.parent().map(|dir| dir.to_path_buf())
        }
        Some(_) | None => None,
    };
    env
}

fn depot_path_without_revision(file: &UniversalPath) -> Result<&str, String> {
    match file {
        UniversalPath::Depot(path, _rev) => Ok(path),
        _ => Err(format!("{file} is not a depot path")),
    }
}

/// A folder query also gets records for view lines under the folder, so only records for exactly
/// `local` count; of those the last wins, as with `parse_where_local_path`.
fn depot_path_of_where(p4: &impl P4Runner, local: &Path) -> Result<String, String> {
    let normalize = |path: &str| path.replace('/', "\\").to_lowercase();
    let queried = local.to_string_lossy();
    let out = p4.run(&["-ztag", "where", &escape_local_path(&queried)])?;
    let records = ztag_records(&out);
    let last = records
        .iter()
        .rev()
        .find(|r| ztag_field(r, "path").is_some_and(|p| normalize(p) == normalize(&queried)));
    match last {
        Some(record) if ztag_field(record, "unmap").is_none() => {
            ztag_field(record, "depotFile").map(str::to_owned)
        }
        _ => None,
    }
    .ok_or_else(|| format!("{} is not mapped to the p4 workspace", local.display()))
}

pub fn depot_file_of_local(p4: &impl P4Runner, file: &Path) -> Result<String, String> {
    depot_path_of_where(p4, file)
}

/// Run `p4` with `folder` as cwd so a P4CONFIG file at the workspace root is found.
pub fn depot_folder_of_local(p4: &impl P4Runner, folder: &Path) -> Result<String, String> {
    let tree = depot_path_of_where(p4, &folder.join("..."))?;
    tree.strip_suffix("/...").map(str::to_owned).ok_or_else(|| {
        format!(
            "p4 where {}: unexpected depot path {tree}",
            folder.display()
        )
    })
}

/// The local path in `p4 -ztag where` output, or `None` when the file isn't mapped. Tagged output
/// because the plain one separates its three paths with spaces, which paths can contain. With
/// several records (overlay and exclusion lines of the client view) the last one decides, as the
/// later view line wins; an excluded one carries an `unmap` field.
pub fn parse_where_local_path(ztag: &str) -> Option<PathBuf> {
    let mut last = None;
    let mut path = None;
    let mut unmap = false;
    // A blank line after the last record is optional.
    for line in ztag.lines().chain([""]) {
        if line.trim().is_empty() {
            if path.is_some() || unmap {
                last = Some((path.take(), unmap));
            }
            unmap = false;
        } else if let Some(value) = line.strip_prefix("... path ") {
            path = Some(PathBuf::from(value));
        } else if line.trim_end() == "... unmap" || line.starts_with("... unmap ") {
            unmap = true;
        }
    }
    match last {
        Some((path, false)) => path,
        _ => None,
    }
}

/// The workspace file a depot file maps to, per `p4 where`. Whether it exists isn't checked.
pub fn local_path_of_depot_file(
    depot_file: &UniversalPath,
    p4: &impl P4Runner,
) -> Result<PathBuf, String> {
    let depot_path = depot_path_without_revision(depot_file)?;
    let out = p4.run(&["-ztag", "where", depot_path])?;
    parse_where_local_path(&out)
        .ok_or_else(|| format!("{depot_path} is not mapped to the workspace"))
}

/// The `key value` fields of each record in tagged output; the value is the rest of the line.
fn ztag_records(ztag: &str) -> Vec<Vec<(&str, &str)>> {
    let mut records = Vec::new();
    let mut fields = Vec::new();
    for line in ztag.lines().chain([""]) {
        if let Some(field) = line.strip_prefix("... ") {
            fields.push(field.split_once(' ').unwrap_or((field, "")));
        } else if line.trim().is_empty() && !fields.is_empty() {
            records.push(std::mem::take(&mut fields));
        }
    }
    records
}

fn ztag_field<'a>(record: &[(&str, &'a str)], key: &str) -> Option<&'a str> {
    record
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, value)| *value)
}

pub fn parse_dirs(ztag: &str) -> Vec<String> {
    ztag_records(ztag)
        .iter()
        .filter_map(|record| ztag_field(record, "dir"))
        .map(str::to_owned)
        .collect()
}

pub fn parse_files_at_head(ztag: &str) -> Vec<String> {
    ztag_records(ztag)
        .iter()
        .filter(|record| !is_delete_action(ztag_field(record, "action")))
        .filter_map(|record| ztag_field(record, "depotFile"))
        .map(str::to_owned)
        .collect()
}

fn is_delete_action(action: Option<&str>) -> bool {
    matches!(action, Some("delete" | "move/delete"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P4Stream {
    pub stream: String,
    pub name: String,
    pub kind: String,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P4FileDigest {
    pub depot_file: String,
    pub rev: u32,
    pub size: Option<u64>,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P4ShelvedFile {
    pub depot_file: String,
    pub action: String,
    pub base_rev: Option<u32>,
    pub size: Option<u64>,
    pub digest: Option<String>,
}

fn ztag_fields(ztag: &str) -> impl Iterator<Item = (&str, &str)> {
    ztag.lines()
        .filter_map(|line| line.strip_prefix("... "))
        .map(|field| field.split_once(' ').unwrap_or((field, "")))
}

fn parse_number<T: str::FromStr>(value: &str, key: &str, file: &str) -> Result<T, String> {
    value
        .parse()
        .map_err(|_| format!("{file}: {key} {value:?} is not a number"))
}

pub fn parse_streams(ztag: &str) -> Result<Vec<P4Stream>, String> {
    // Records are split at each Stream field, not at blank lines: a Description can contain them.
    let mut records: Vec<Vec<(&str, &str)>> = Vec::new();
    for (key, value) in ztag_fields(ztag) {
        if key == "Stream" {
            records.push(Vec::new());
        }
        let Some(record) = records.last_mut() else {
            return Err(format!("{key} before any Stream"));
        };
        record.push((key, value));
    }
    records
        .iter()
        .map(|record| {
            let stream = ztag_field(record, "Stream").unwrap_or_default();
            let required = |key| {
                ztag_field(record, key)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{stream}: no {key}"))
            };
            Ok(P4Stream {
                stream: stream.to_owned(),
                name: required("Name")?,
                kind: required("Type")?,
                parent: ztag_field(record, "Parent")
                    .filter(|parent| *parent != "none")
                    .map(str::to_owned),
            })
        })
        .collect()
}

pub fn parse_fstat_digests(ztag: &str) -> Result<Vec<P4FileDigest>, String> {
    ztag_records(ztag)
        .iter()
        // Without a headAction the file is only opened for add in the client.
        .filter(|record| {
            let head_action = ztag_field(record, "headAction");
            head_action.is_some() && !is_delete_action(head_action)
        })
        .map(|record| {
            let depot_file = ztag_field(record, "depotFile")
                .ok_or_else(|| format!("a file without depotFile: {record:?}"))?;
            let rev =
                ztag_field(record, "headRev").ok_or_else(|| format!("{depot_file}: no headRev"))?;
            Ok(P4FileDigest {
                depot_file: depot_file.to_owned(),
                rev: parse_number(rev, "headRev", depot_file)?,
                size: ztag_field(record, "fileSize")
                    .map(|size| parse_number(size, "fileSize", depot_file))
                    .transpose()?,
                digest: ztag_field(record, "digest").map(str::to_owned),
            })
        })
        .collect()
}

/// `p4 describe` output is one record whose files are numbered fields (`depotFile0`, `rev0`, ...)
/// after a `desc` that can contain blank lines, so fields are gathered by number, not by record.
pub fn parse_shelf(ztag: &str) -> Result<Vec<P4ShelvedFile>, String> {
    let mut files: std::collections::BTreeMap<u32, [Option<&str>; 5]> = Default::default();
    for (key, value) in ztag_fields(ztag) {
        let name = key.trim_end_matches(|c: char| c.is_ascii_digit());
        let slot = match name {
            "depotFile" => 0,
            "action" => 1,
            "rev" => 2,
            "fileSize" => 3,
            "digest" => 4,
            _ => continue,
        };
        let Ok(index) = key[name.len()..].parse::<u32>() else {
            continue;
        };
        files.entry(index).or_default()[slot] = Some(value);
    }
    files
        .into_iter()
        .map(|(index, [depot_file, action, rev, size, digest])| {
            let depot_file = depot_file.ok_or_else(|| format!("no depotFile{index}"))?;
            let action = action.ok_or_else(|| format!("{depot_file}: no action{index}"))?;
            let rev = rev.ok_or_else(|| format!("{depot_file}: no rev{index}"))?;
            // A new file's rev is the one it will get, not one the depot path has.
            let base_rev = match action {
                "add" | "move/add" | "branch" | "import" => None,
                _ => Some(parse_number(rev, "rev", depot_file)?),
            };
            Ok(P4ShelvedFile {
                depot_file: depot_file.to_owned(),
                action: action.to_owned(),
                base_rev,
                size: size
                    .map(|size| parse_number(size, "fileSize", depot_file))
                    .transpose()?,
                digest: digest.map(str::to_owned),
            })
        })
        .collect()
}

fn command_error(args: &[&str], error: String) -> String {
    format!("p4 {}: {error}", args.join(" "))
}

fn run_parsed<T>(
    p4: &impl P4Runner,
    args: &[&str],
    parse: fn(&str) -> Result<T, String>,
) -> Result<T, String> {
    p4.run(args)
        .and_then(|out| parse(&out))
        .map_err(|e| command_error(args, e))
}

pub fn list_streams(p4: &impl P4Runner) -> Result<Vec<P4Stream>, String> {
    run_parsed(
        p4,
        &["-ztag", "streams", "-T", "Stream,Name,Parent,Type"],
        parse_streams,
    )
}

/// Files under `depot_folder` at `revision` (`""` for head, or `@change`), deleted ones left out.
pub fn list_files_with_digests(
    p4: &impl P4Runner,
    depot_folder: &str,
    revision: &str,
) -> Result<Vec<P4FileDigest>, String> {
    let files = format!("{}/...{revision}", depot_folder.trim_end_matches('/'));
    run_parsed(p4, &["-ztag", "fstat", "-Ol", &files], parse_fstat_digests)
}

pub fn list_shelf(p4: &impl P4Runner, change: u32) -> Result<Vec<P4ShelvedFile>, String> {
    let change = change.to_string();
    run_parsed(p4, &["-ztag", "describe", "-S", "-s", &change], parse_shelf)
}

/// `revision` is `""`, `#n`, `@change` or `@=shelf`.
pub fn print_file(p4: &impl P4Runner, depot_path: &str, revision: &str) -> Result<Vec<u8>, String> {
    let file = format!("{depot_path}{revision}");
    let args = ["print", "-q", &file];
    p4.run_bytes(&args).map_err(|e| command_error(&args, e))
}

/// What a failed command reports: stderr, else stdout, else the exit status.
fn failure_text(stderr: &[u8], stdout: &[u8], status: impl std::fmt::Display) -> String {
    [stderr, stdout]
        .iter()
        .map(|text| String::from_utf8_lossy(text).trim().to_string())
        .find(|text| !text.is_empty())
        .unwrap_or_else(|| format!("p4 failed ({status})"))
}

pub struct P4Command {
    exe_path: String,
    is_gui: bool,
    file: Option<UniversalPath>,
}

impl P4Command {
    pub fn new(is_gui: bool) -> Self {
        let mut exe_path = env::var("P4PATH").unwrap_or_else(|_| "p4.exe".to_string());
        if is_gui {
            exe_path = exe_path.replace("p4.exe", "p4vc.bat")
        }

        Self {
            exe_path,
            is_gui: is_gui,
            file: None,
        }
    }

    /// The file the command is about, so it runs with that file's environment.
    pub fn for_file(mut self, file: UniversalPath) -> Self {
        self.file = Some(file);
        self
    }

    fn build_cmd(&self, config: &P4Config) -> Command {
        let mut cmd = Command::new(&self.exe_path);
        let env = resolve_p4_env(config, self.file.as_ref(), self.is_gui);
        // Global options must come before the command, which the caller appends.
        cmd.args(env.args);
        for (var, value) in env.set {
            cmd.env(var, value);
        }
        for var in env.unset {
            cmd.env_remove(var);
        }
        if let Some(cwd) = env.cwd {
            cmd.current_dir(cwd);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console flashing up per spawn from the GUI apps.
            cmd.creation_flags(0x0800_0000);
        }
        cmd
    }

    fn prepare_cmd(&self) -> Command {
        self.build_cmd(&get_p4_config())
    }

    pub fn output(&self, args: &[&str]) -> Result<String, String> {
        String::from_utf8(self.output_bytes(args)?).map_err(|e| e.to_string())
    }

    /// Like `output`, but stdout as raw bytes, for file content that may not be text.
    pub fn output_bytes(&self, args: &[&str]) -> Result<Vec<u8>, String> {
        let output = self
            .prepare_cmd()
            .args(args)
            .output()
            .map_err(|e| format!("{}: {e}", self.exe_path))?;

        if output.status.success() {
            // p4 exits 0 on some warnings ("no such file(s)", "not in client view").
            let warnings = String::from_utf8_lossy(&output.stderr);
            if !warnings.trim().is_empty() {
                log::warn!("p4 {}: {}", args.join(" "), warnings.trim());
            }
            Ok(output.stdout)
        } else {
            Err(failure_text(&output.stderr, &output.stdout, output.status))
        }
    }

    pub fn spawn(&self, args: &[&str]) -> Result<Child, String> {
        self.prepare_cmd()
            .args(args)
            .spawn()
            .map_err(|e| format!("{}: {e}", self.exe_path))
    }

    pub fn get_depot_file_bytes(file: &UniversalPath) -> Result<Vec<u8>, String> {
        P4Command::new(false).for_file(file.clone()).output_bytes(&[
            "print",
            "-q",
            &file.to_p4_string(),
        ])
    }
    pub fn open_revision_graph(file: &UniversalPath) -> Result<(), String> {
        let path = depot_path_without_revision(file)?;
        P4Command::new(true)
            .for_file(file.clone())
            .spawn(&["revisiongraph", path])?;
        Ok(())
    }
    pub fn open_timelapse_view(file: &UniversalPath) -> Result<(), String> {
        let path = depot_path_without_revision(file)?;
        P4Command::new(true)
            .for_file(file.clone())
            .spawn(&["timelapse", path])?;
        Ok(())
    }
    #[allow(dead_code)]
    pub fn get_revision_history(path: &str) -> Result<String, String> {
        P4Command::new(false).output(&["-ztag", "filelog", "-m", "10", path])
    }
}

#[allow(dead_code)]
pub struct P4Revision {
    pub rev: u32,
    pub change: u32,
    pub action: String,
    pub date: String,
}

/// A p4 runner for tests: answers through a closure and records every call.
#[cfg(any(test, feature = "test-util"))]
pub struct FakeP4<F: Fn(&[&str]) -> Result<String, String>> {
    respond: F,
    calls: std::cell::RefCell<Vec<Vec<String>>>,
}

#[cfg(any(test, feature = "test-util"))]
impl<F: Fn(&[&str]) -> Result<String, String>> FakeP4<F> {
    pub fn new(respond: F) -> Self {
        Self {
            respond,
            calls: Default::default(),
        }
    }

    pub fn calls(&self) -> Vec<Vec<String>> {
        self.calls.borrow().clone()
    }
}

#[cfg(any(test, feature = "test-util"))]
impl<F: Fn(&[&str]) -> Result<String, String>> P4Runner for FakeP4<F> {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|a| a.to_string()).collect());
        (self.respond)(args)
    }
}

#[cfg(test)]
mod tests {
    use crate::universal_path::DepotRev;
    use std::ffi::OsStr;

    use super::*;

    fn config(port: &str, user: &str, client: &str, charset: &str, password: &str) -> P4Config {
        P4Config {
            port: port.into(),
            user: user.into(),
            client: client.into(),
            charset: charset.into(),
            password: password.into(),
        }
    }

    fn two_profiles(slot: P4Config, default: P4Config) -> P4Profiles {
        P4Profiles {
            profiles: vec![
                P4Profile {
                    id: 1,
                    name: "slot".into(),
                    config: slot,
                },
                P4Profile {
                    id: 2,
                    name: "default".into(),
                    config: default,
                },
            ],
            default: Some(2),
            next_id: 3,
        }
    }

    /// The global options and unset variables a p4 command runs with for a slot.
    fn command_shape(profiles: &P4Profiles, slot: Option<u64>) -> (Vec<String>, Vec<&str>) {
        let env = resolve_p4_env(&profiles.resolve(slot), None, false);
        assert!(env.set.is_empty(), "p4 gets flags, not variables");
        (env.args, env.unset)
    }

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn slot_profile_over_default_profile_over_environment() {
        let profiles = two_profiles(
            config("slot:1666", "", "", "", ""),
            config("default:1666", "me", "", "", ""),
        );
        let (args_, unset) = command_shape(&profiles, Some(1));
        assert_eq!(args_, args(&["-p", "slot:1666", "-u", "me"]));
        assert!(unset.is_empty());
    }

    #[test]
    fn empty_slot_fields_fall_through_to_the_default_then_the_environment() {
        let profiles = two_profiles(config("", "", "", "", ""), config("", "me", "", "utf8", ""));
        let (args_, unset) = command_shape(&profiles, Some(1));
        assert_eq!(args_, args(&["-u", "me", "-C", "utf8"]));
        assert!(unset.is_empty());

        let empty = two_profiles(P4Config::default(), P4Config::default());
        assert_eq!(command_shape(&empty, Some(1)), (vec![], vec![]));
    }

    #[test]
    fn hash_in_the_slot_unsets_even_when_the_default_has_a_value() {
        let profiles = two_profiles(
            config("", "#", "#", "", ""),
            config("", "me", "ws", "", "pw"),
        );
        let (args_, unset) = command_shape(&profiles, Some(1));
        assert_eq!(args_, args(&["-P", "pw"]));
        assert_eq!(unset, vec!["P4USER", "P4CLIENT"]);
    }

    #[test]
    fn auto_uses_the_default_profile_and_the_environment_only() {
        let profiles = two_profiles(
            config("slot:1666", "slot_user", "", "", ""),
            config("", "me", "", "", ""),
        );
        let (args_, unset) = command_shape(&profiles, None);
        assert_eq!(args_, args(&["-u", "me"]));
        assert!(unset.is_empty());
    }

    #[test]
    fn a_removed_slot_profile_is_auto_and_a_removed_default_is_the_environment() {
        let mut profiles = two_profiles(
            config("slot:1666", "", "", "", ""),
            config("", "me", "", "", ""),
        );
        profiles.remove(1);
        assert_eq!(command_shape(&profiles, Some(1)).0, args(&["-u", "me"]));

        profiles.remove(2);
        assert_eq!(profiles.default, None);
        assert_eq!(command_shape(&profiles, Some(1)), (vec![], vec![]));
        assert_eq!(command_shape(&profiles, None), (vec![], vec![]));
    }

    #[test]
    fn a_dangling_default_is_the_environment() {
        let mut profiles = two_profiles(P4Config::default(), config("", "me", "", "", ""));
        profiles.default = Some(7);
        assert_eq!(command_shape(&profiles, None), (vec![], vec![]));
    }

    #[test]
    fn added_profiles_get_fresh_ids() {
        let mut profiles = P4Profiles::default();
        let a = profiles.add("a".into());
        let b = profiles.add("b".into());
        assert_ne!(a, b);
        assert_eq!(profiles.get(b).unwrap().name, "b");
        assert_eq!(profiles.get(b).unwrap().config, P4Config::default());

        profiles.remove(a);
        assert!(profiles.get(a).is_none());
        let c = profiles.add("c".into());
        assert_ne!(c, b);
        assert_eq!(profiles.profiles.len(), 2);
    }

    #[test]
    fn a_removed_id_is_never_reused_so_its_slots_stay_auto() {
        let mut profiles = P4Profiles::default();
        profiles.add("a".into());
        let b = profiles.add("b".into());
        profiles.remove(b);

        let c = profiles.add("c".into());
        profiles.profiles.last_mut().unwrap().config.port = "c:1666".into();
        assert_ne!(c, b);
        assert_eq!(profiles.resolve(Some(b)), P4Config::default());
    }

    #[test]
    fn p4vc_gets_variables_instead_of_flags() {
        let env = resolve_p4_env(&config("ssl:p4:1666", "#", "", "", "pw"), None, true);
        assert!(env.args.is_empty());
        assert_eq!(
            env.set,
            vec![
                ("P4PORT", "ssl:p4:1666".to_string()),
                ("P4PASSWD", "pw".to_string()),
            ]
        );
        assert_eq!(env.unset, vec!["P4USER"]);
    }

    #[test]
    fn non_empty_fields_are_passed_as_global_options() {
        let env = resolve_p4_env(
            &config("ssl:p4:1666", "me", "ws", "utf8", "pw"),
            None,
            false,
        );
        assert_eq!(
            env.args,
            args(&[
                "-p",
                "ssl:p4:1666",
                "-u",
                "me",
                "-P",
                "pw",
                "-c",
                "ws",
                "-C",
                "utf8"
            ])
        );
        assert!(env.set.is_empty());
        assert!(env.unset.is_empty());
    }

    #[test]
    fn empty_fields_are_left_to_the_environment() {
        assert_eq!(
            resolve_p4_env(&P4Config::default(), None, false),
            P4Env::default()
        );
        assert_eq!(
            resolve_p4_env(&P4Config::default(), None, true),
            P4Env::default()
        );

        let env = resolve_p4_env(&config("", "me", "", "", ""), None, false);
        assert_eq!(env.args, args(&["-u", "me"]));
        assert!(env.unset.is_empty());
    }

    #[test]
    fn hash_unsets_the_variable() {
        let env = resolve_p4_env(&config("#", "me", "#", "", "#"), None, false);
        assert_eq!(env.args, args(&["-u", "me"]));
        assert_eq!(env.unset, vec!["P4PORT", "P4PASSWD", "P4CLIENT"]);
    }

    #[test]
    fn a_local_file_runs_in_its_directory() {
        let dir = env::temp_dir().join("repo").join("src");
        let file = UniversalPath::Local(dir.join("a.txt"));
        let env = resolve_p4_env(&P4Config::default(), Some(&file), false);
        assert_eq!(env.cwd, Some(dir));
    }

    #[test]
    fn a_depot_path_a_relative_path_or_no_file_keep_the_cwd() {
        let depot = UniversalPath::Depot("//depot/main/a.txt".into(), Some(DepotRev::Rev(3)));
        let bare = UniversalPath::Local(PathBuf::from("a.txt"));
        let relative = UniversalPath::Local(PathBuf::from("sub").join("a.txt"));
        for file in [Some(&depot), Some(&bare), Some(&relative), None] {
            assert_eq!(
                resolve_p4_env(&P4Config::default(), file, false).cwd,
                None,
                "{file:?}"
            );
        }
    }

    #[test]
    fn the_command_passes_options_before_the_command_removes_and_runs_in_the_directory() {
        let dir = env::temp_dir().join("ws");
        let mut cmd = P4Command::new(false)
            .for_file(UniversalPath::Local(dir.join("a.txt")))
            .build_cmd(&config("ssl:p4:1666", "#", "", "", ""));
        cmd.args(["print", "-q", "a.txt"]);
        let cmd_args: Vec<_> = cmd.get_args().collect();
        assert_eq!(cmd_args, ["-p", "ssl:p4:1666", "print", "-q", "a.txt"]);
        let envs: Vec<_> = cmd.get_envs().collect();
        assert_eq!(envs, vec![(OsStr::new("P4USER"), None)]);
        assert_eq!(cmd.get_current_dir(), Some(dir.as_path()));

        let cmd = P4Command::new(false).build_cmd(&P4Config::default());
        assert_eq!(cmd.get_args().count(), 0);
        assert_eq!(cmd.get_envs().count(), 0);
        assert_eq!(cmd.get_current_dir(), None);
    }

    #[test]
    fn p4vc_commands_set_variables() {
        let cmd = P4Command::new(true).build_cmd(&config("ssl:p4:1666", "#", "", "", ""));
        assert_eq!(cmd.get_args().count(), 0);
        let envs: Vec<_> = cmd.get_envs().collect();
        assert_eq!(
            envs,
            vec![
                (OsStr::new("P4PORT"), Some(OsStr::new("ssl:p4:1666"))),
                (OsStr::new("P4USER"), None),
            ]
        );
    }

    #[test]
    fn failure_text_is_stderr_then_stdout_then_the_exit_status() {
        assert_eq!(failure_text(b"err\n", b"out\n", "exit code: 1"), "err");
        assert_eq!(failure_text(b"  ", b"out\n", "exit code: 1"), "out");
        assert_eq!(
            failure_text(b"", b"", "exit code: 1"),
            "p4 failed (exit code: 1)"
        );
    }

    const WHERE_SPACES: &str = "... depotFile //depot/main/my dir/a b.txt\n\
                                ... clientFile //ws/main/my dir/a b.txt\n\
                                ... path C:\\ws\\main\\my dir\\a b.txt\n\
                                \n";

    #[test]
    fn where_output_gives_the_local_path_with_spaces() {
        assert_eq!(
            parse_where_local_path(WHERE_SPACES),
            Some(PathBuf::from(r"C:\ws\main\my dir\a b.txt"))
        );
        let crlf = WHERE_SPACES.replace('\n', "\r\n");
        assert_eq!(
            parse_where_local_path(&crlf),
            Some(PathBuf::from(r"C:\ws\main\my dir\a b.txt"))
        );
    }

    #[test]
    fn where_output_without_a_mapping_gives_none() {
        assert_eq!(parse_where_local_path(""), None);
        assert_eq!(parse_where_local_path("\r\n"), None);
        let excluded = "... depotFile //depot/main/a.txt\n\
                        ... clientFile //ws/main/a.txt\n\
                        ... path C:\\ws\\main\\a.txt\n\
                        ... unmap \n";
        assert_eq!(parse_where_local_path(excluded), None);
    }

    #[test]
    fn where_output_with_several_records_takes_the_last() {
        let mapped = "... depotFile //depot/main/a.txt\n\
                      ... clientFile //ws/main/a.txt\n\
                      ... path C:\\ws\\main\\a.txt\n";
        let overlay = "... depotFile //depot/main/a.txt\n\
                       ... clientFile //ws/overlay/a.txt\n\
                       ... path C:\\ws\\overlay\\a.txt\n";
        let excluded = "... depotFile //depot/main/a.txt\n\
                        ... clientFile //ws/main/a.txt\n\
                        ... path C:\\ws\\main\\a.txt\n\
                        ... unmap\n";
        assert_eq!(
            parse_where_local_path(&format!("{mapped}\n{overlay}\n")),
            Some(PathBuf::from(r"C:\ws\overlay\a.txt"))
        );
        assert_eq!(
            parse_where_local_path(&format!("{mapped}\n{excluded}\n")),
            None
        );
        assert_eq!(
            parse_where_local_path(&format!("{excluded}\n{mapped}\n")),
            Some(PathBuf::from(r"C:\ws\main\a.txt"))
        );
    }

    #[test]
    fn depot_file_of_a_local_file_asks_p4_where_with_the_escaped_path() {
        let p4 = FakeP4::new(|_: &[&str]| Ok(WHERE_SPACES.to_string()));
        assert_eq!(
            depot_file_of_local(&p4, Path::new(r"C:\ws\main\my dir\a b.txt")),
            Ok("//depot/main/my dir/a b.txt".to_string())
        );
        let p4 = FakeP4::new(|_: &[&str]| Ok(String::new()));
        let _ = depot_file_of_local(&p4, Path::new(r"C:\ws\a@1.txt"));
        assert_eq!(
            p4.calls(),
            vec![args(&["-ztag", "where", r"C:\ws\a%401.txt"])]
        );
    }

    #[test]
    fn depot_folder_of_a_local_folder_asks_for_its_tree_and_drops_the_wildcard() {
        let ztag = "... depotFile //depot/main/src/...\n\
                    ... clientFile //ws/src/...\n\
                    ... path C:\\ws\\src\\...\n";
        let p4 = FakeP4::new(|_: &[&str]| Ok(ztag.to_string()));
        assert_eq!(
            depot_folder_of_local(&p4, Path::new(r"C:\ws\src")),
            Ok("//depot/main/src".to_string())
        );
        assert_eq!(
            p4.calls(),
            vec![args(&["-ztag", "where", r"C:\ws\src\..."])]
        );
    }

    #[test]
    fn depot_folder_ignores_records_for_excluded_subfolders() {
        let ztag = "... depotFile //depot/main/src/...\n\
                    ... clientFile //ws/src/...\n\
                    ... path C:\\ws\\src\\...\n\
                    \n\
                    ... depotFile -//depot/main/src/tmp/...\n\
                    ... clientFile -//ws/src/tmp/...\n\
                    ... path C:\\ws\\src\\tmp\\...\n\
                    ... unmap\n";
        let p4 = FakeP4::new(|_: &[&str]| Ok(ztag.to_string()));
        assert_eq!(
            depot_folder_of_local(&p4, Path::new(r"C:\ws\SRC")),
            Ok("//depot/main/src".to_string())
        );
    }

    #[test]
    fn depot_folder_of_an_excluded_folder_is_an_error() {
        let ztag = "... depotFile //depot/main/src/...\n\
                    ... clientFile //ws/src/...\n\
                    ... path C:\\ws\\src\\...\n\
                    \n\
                    ... depotFile -//depot/main/src/...\n\
                    ... clientFile -//ws/src/...\n\
                    ... path C:\\ws\\src\\...\n\
                    ... unmap\n";
        let p4 = FakeP4::new(|_: &[&str]| Ok(ztag.to_string()));
        let err = depot_folder_of_local(&p4, Path::new(r"C:\ws\src")).unwrap_err();
        assert!(err.contains(r"C:\ws\src"), "{err}");
    }

    #[test]
    fn depot_path_of_an_unmapped_local_path_is_an_error() {
        let p4 = FakeP4::new(|_: &[&str]| Ok(String::new()));
        let err = depot_file_of_local(&p4, Path::new(r"C:\ws\a.txt")).unwrap_err();
        assert!(err.contains(r"C:\ws\a.txt"), "{err}");

        let p4 = FakeP4::new(|_: &[&str]| Err("not under client's root".to_string()));
        let err = depot_folder_of_local(&p4, Path::new(r"D:\x")).unwrap_err();
        assert!(err.contains("not under client's root"), "{err}");
    }

    #[test]
    fn local_path_of_a_depot_file_asks_p4_where_without_the_revision() {
        let p4 = FakeP4::new(|_: &[&str]| Ok(WHERE_SPACES.to_string()));
        let depot =
            UniversalPath::Depot("//depot/main/my dir/a b.txt".into(), Some(DepotRev::Rev(3)));
        assert_eq!(
            local_path_of_depot_file(&depot, &p4),
            Ok(PathBuf::from(r"C:\ws\main\my dir\a b.txt"))
        );
        assert_eq!(
            p4.calls(),
            vec![args(&["-ztag", "where", "//depot/main/my dir/a b.txt"])]
        );
    }

    #[test]
    fn local_path_of_an_unmapped_depot_file_is_an_error() {
        let depot = UniversalPath::Depot("//depot/main/a.txt".into(), None);
        // p4 exits 0 on "not in client view": empty stdout.
        let p4 = FakeP4::new(|_: &[&str]| Ok(String::new()));
        let err = local_path_of_depot_file(&depot, &p4).unwrap_err();
        assert!(err.contains("//depot/main/a.txt"), "{err}");

        let p4 = FakeP4::new(|_: &[&str]| Err("no such file(s)".to_string()));
        let err = local_path_of_depot_file(&depot, &p4).unwrap_err();
        assert!(err.contains("no such file(s)"), "{err}");
    }

    #[test]
    fn local_path_of_a_local_file_is_an_error_without_running_p4() {
        let p4 = FakeP4::new(|_: &[&str]| panic!("p4 must not run"));
        let local = UniversalPath::Local(PathBuf::from(r"C:\ws\a.txt"));
        assert!(local_path_of_depot_file(&local, &p4).is_err());
    }

    #[test]
    fn dirs_output_gives_the_folders_with_spaces() {
        let ztag = "... dir //depot/main/my src\n\
                    \n\
                    ... dir //depot/main/docs\n";
        assert_eq!(
            parse_dirs(ztag),
            ["//depot/main/my src", "//depot/main/docs"]
        );
        assert_eq!(
            parse_dirs(&ztag.replace('\n', "\r\n")),
            ["//depot/main/my src", "//depot/main/docs"]
        );
        assert!(parse_dirs("").is_empty());
    }

    #[test]
    fn files_output_leaves_out_files_deleted_at_head() {
        let record = |path: &str, action: &str| {
            format!(
                "... depotFile {path}\n\
                 ... rev 4\n\
                 ... change 1234\n\
                 ... action {action}\n\
                 ... type text\n\
                 ... time 1700000000\n\
                 \n"
            )
        };
        let ztag: String = [
            ("//depot/main/a b.txt", "edit"),
            ("//depot/main/gone.txt", "delete"),
            ("//depot/main/added.txt", "add"),
            ("//depot/main/moved_away.txt", "move/delete"),
            ("//depot/main/moved_here.txt", "move/add"),
            ("//depot/main/branched.txt", "branch"),
            ("//depot/main/merged.txt", "integrate"),
        ]
        .iter()
        .map(|(path, action)| record(path, action))
        .collect();
        let live = [
            "//depot/main/a b.txt",
            "//depot/main/added.txt",
            "//depot/main/moved_here.txt",
            "//depot/main/branched.txt",
            "//depot/main/merged.txt",
        ];
        assert_eq!(parse_files_at_head(&ztag), live);
        assert_eq!(parse_files_at_head(&ztag.replace('\n', "\r\n")), live);
        assert_eq!(
            parse_files_at_head(ztag.trim_end()),
            live,
            "no blank line after the last record"
        );
        assert!(parse_files_at_head("").is_empty());
    }

    const STREAMS: &str = "... Stream //streams/main\n\
                           ... Update 2024/01/02 10:00:00\n\
                           ... Owner alice\n\
                           ... Name main\n\
                           ... Parent none\n\
                           ... Type mainline\n\
                           ... Description The mainline.\n\
                           \n\
                           A second paragraph.\n\
                           ... Options allsubmit unlocked toparent fromparent mergedown\n\
                           \n\
                           ... Stream //streams/dev feature\n\
                           ... Name dev feature\n\
                           ... Parent //streams/main\n\
                           ... Type development\n\
                           \n\
                           ... Stream //streams/rel-1.0\n\
                           ... Name rel-1.0\n\
                           ... Parent //streams/main\n\
                           ... Type release\n\
                           \n";

    fn stream(stream: &str, name: &str, kind: &str, parent: Option<&str>) -> P4Stream {
        P4Stream {
            stream: stream.into(),
            name: name.into(),
            kind: kind.into(),
            parent: parent.map(str::to_owned),
        }
    }

    #[test]
    fn streams_output_gives_each_stream_with_its_parent() {
        let streams = vec![
            stream("//streams/main", "main", "mainline", None),
            stream(
                "//streams/dev feature",
                "dev feature",
                "development",
                Some("//streams/main"),
            ),
            stream(
                "//streams/rel-1.0",
                "rel-1.0",
                "release",
                Some("//streams/main"),
            ),
        ];
        assert_eq!(parse_streams(STREAMS), Ok(streams.clone()));
        assert_eq!(
            parse_streams(&STREAMS.replace('\n', "\r\n")),
            Ok(streams.clone())
        );
        assert_eq!(parse_streams(STREAMS.trim_end()), Ok(streams));
        assert_eq!(parse_streams(""), Ok(vec![]));
    }

    #[test]
    fn malformed_streams_output_is_an_error() {
        let no_name = "... Stream //streams/main\n... Type mainline\n";
        let err = parse_streams(no_name).unwrap_err();
        assert!(err.contains("//streams/main"), "{err}");
        assert!(parse_streams("... Name main\n... Stream //streams/main\n").is_err());
    }

    const FSTAT: &str = "... depotFile //depot/main/a b.txt\n\
                         ... clientFile //ws/main/a b.txt\n\
                         ... isMapped \n\
                         ... headAction edit\n\
                         ... headType text\n\
                         ... headTime 1700000000\n\
                         ... headRev 4\n\
                         ... headChange 1234\n\
                         ... headModTime 1699999999\n\
                         ... fileSize 1234\n\
                         ... digest 0CC175B9C0F1B6A831C399E269772661\n\
                         ... ... otherOpen0 bob@bob_ws\n\
                         ... ... otherAction0 edit\n\
                         \n\
                         ... depotFile //depot/main/gone.txt\n\
                         ... headAction delete\n\
                         ... headType text\n\
                         ... headRev 3\n\
                         ... headChange 1200\n\
                         \n\
                         ... depotFile //depot/main/moved_away.txt\n\
                         ... headAction move/delete\n\
                         ... headType text\n\
                         ... headRev 2\n\
                         ... headChange 1201\n\
                         \n\
                         ... depotFile //depot/main/opened_for_delete.txt\n\
                         ... headAction add\n\
                         ... headType binary\n\
                         ... headRev 1\n\
                         ... headChange 1100\n\
                         ... fileSize 0\n\
                         ... digest D41D8CD98F00B204E9800998ECF8427E\n\
                         ... action delete\n\
                         \n\
                         ... depotFile //depot/main/only_opened_for_add.txt\n\
                         ... action add\n\
                         ... type text\n\
                         \n\
                         ... depotFile //depot/main/archived.bin\n\
                         ... headAction archive\n\
                         ... headType binary+S\n\
                         ... headRev 9\n\
                         ... headChange 1000\n\
                         \n";

    fn digest(depot_file: &str, rev: u32, size: Option<u64>, digest: Option<&str>) -> P4FileDigest {
        P4FileDigest {
            depot_file: depot_file.into(),
            rev,
            size,
            digest: digest.map(str::to_owned),
        }
    }

    #[test]
    fn fstat_output_gives_sizes_and_digests_without_deleted_files() {
        let files = vec![
            digest(
                "//depot/main/a b.txt",
                4,
                Some(1234),
                Some("0CC175B9C0F1B6A831C399E269772661"),
            ),
            digest(
                "//depot/main/opened_for_delete.txt",
                1,
                Some(0),
                Some("D41D8CD98F00B204E9800998ECF8427E"),
            ),
            digest("//depot/main/archived.bin", 9, None, None),
        ];
        assert_eq!(parse_fstat_digests(FSTAT), Ok(files.clone()));
        assert_eq!(
            parse_fstat_digests(&FSTAT.replace('\n', "\r\n")),
            Ok(files.clone())
        );
        assert_eq!(parse_fstat_digests(FSTAT.trim_end()), Ok(files));
        assert_eq!(parse_fstat_digests(""), Ok(vec![]));
    }

    #[test]
    fn malformed_fstat_output_is_an_error() {
        let bad_rev = "... depotFile //depot/main/a.txt\n... headAction edit\n... headRev x\n";
        let err = parse_fstat_digests(bad_rev).unwrap_err();
        assert!(err.contains("//depot/main/a.txt"), "{err}");
        let no_rev = "... depotFile //depot/main/a.txt\n... headAction edit\n";
        assert!(parse_fstat_digests(no_rev).is_err());
        let bad_size = "... depotFile //depot/main/a.txt\n\
                        ... headAction edit\n\
                        ... headRev 1\n\
                        ... fileSize big\n";
        assert!(parse_fstat_digests(bad_size).is_err());
        assert!(parse_fstat_digests("... headAction edit\n... headRev 1\n").is_err());
    }

    const SHELF: &str = "... change 1234\n\
                         ... user alice\n\
                         ... client alice_ws\n\
                         ... time 1700000000\n\
                         ... desc Fix the thing.\n\
                         \n\
                         More detail after a blank line.\n\
                         ... status pending\n\
                         ... changeType public\n\
                         ... path //depot/main/...\n\
                         ... shelved \n\
                         ... depotFile0 //depot/main/a b.txt\n\
                         ... action0 edit\n\
                         ... type0 text\n\
                         ... rev0 4\n\
                         ... fileSize0 120\n\
                         ... digest0 0CC175B9C0F1B6A831C399E269772661\n\
                         ... depotFile1 //depot/main/new.txt\n\
                         ... action1 add\n\
                         ... type1 text\n\
                         ... rev1 1\n\
                         ... depotFile2 //depot/main/old.txt\n\
                         ... action2 delete\n\
                         ... type2 text\n\
                         ... rev2 7\n\
                         ... depotFile3 //depot/main/moved_here.txt\n\
                         ... action3 move/add\n\
                         ... type3 text\n\
                         ... rev3 1\n\
                         ... depotFile4 //depot/main/moved_away.txt\n\
                         ... action4 move/delete\n\
                         ... type4 text\n\
                         ... rev4 3\n\
                         ... depotFile5 //depot/main/branched.txt\n\
                         ... action5 branch\n\
                         ... type5 text\n\
                         ... rev5 1\n\
                         ... depotFile6 //depot/main/merged.txt\n\
                         ... action6 integrate\n\
                         ... type6 text\n\
                         ... rev6 2\n\
                         ... depotFile7 //depot/main/imported.txt\n\
                         ... action7 import\n\
                         ... type7 text\n\
                         ... rev7 1\n\
                         ... depotFile8 //depot/main/b.txt\n\
                         ... action8 edit\n\
                         ... type8 text\n\
                         ... rev8 12\n\
                         ... depotFile9 //depot/main/c.txt\n\
                         ... action9 edit\n\
                         ... type9 text\n\
                         ... rev9 5\n\
                         ... depotFile10 //depot/main/d.txt\n\
                         ... action10 edit\n\
                         ... type10 text\n\
                         ... rev10 6\n\
                         \n";

    fn shelved(depot_file: &str, action: &str, base_rev: Option<u32>) -> P4ShelvedFile {
        P4ShelvedFile {
            depot_file: depot_file.into(),
            action: action.into(),
            base_rev,
            size: None,
            digest: None,
        }
    }

    #[test]
    fn shelf_output_gives_each_file_with_its_base_revision() {
        let files = vec![
            P4ShelvedFile {
                size: Some(120),
                digest: Some("0CC175B9C0F1B6A831C399E269772661".into()),
                ..shelved("//depot/main/a b.txt", "edit", Some(4))
            },
            shelved("//depot/main/new.txt", "add", None),
            shelved("//depot/main/old.txt", "delete", Some(7)),
            shelved("//depot/main/moved_here.txt", "move/add", None),
            shelved("//depot/main/moved_away.txt", "move/delete", Some(3)),
            shelved("//depot/main/branched.txt", "branch", None),
            shelved("//depot/main/merged.txt", "integrate", Some(2)),
            shelved("//depot/main/imported.txt", "import", None),
            shelved("//depot/main/b.txt", "edit", Some(12)),
            shelved("//depot/main/c.txt", "edit", Some(5)),
            shelved("//depot/main/d.txt", "edit", Some(6)),
        ];
        assert_eq!(parse_shelf(SHELF), Ok(files.clone()));
        assert_eq!(parse_shelf(&SHELF.replace('\n', "\r\n")), Ok(files));
        assert_eq!(
            parse_shelf("... change 1234\n... status pending\n"),
            Ok(vec![])
        );
    }

    #[test]
    fn malformed_shelf_output_is_an_error() {
        let no_rev = "... depotFile0 //depot/main/a.txt\n... action0 edit\n";
        let err = parse_shelf(no_rev).unwrap_err();
        assert!(err.contains("//depot/main/a.txt"), "{err}");
        let bad_rev = "... depotFile0 //depot/main/a.txt\n... action0 edit\n... rev0 x\n";
        assert!(parse_shelf(bad_rev).is_err());
        let bad_size =
            "... depotFile0 //depot/main/a.txt\n... action0 edit\n... rev0 1\n... fileSize0 big\n";
        assert!(parse_shelf(bad_size).is_err());
        let no_file = "... action0 edit\n... rev0 1\n";
        assert!(parse_shelf(no_file).is_err());
    }

    #[test]
    fn p4_operations_run_tagged_commands_and_parse_them() {
        let p4 = FakeP4::new(|args: &[&str]| match args[1] {
            "streams" => Ok(STREAMS.to_string()),
            "fstat" => Ok(FSTAT.to_string()),
            "describe" => Ok(SHELF.to_string()),
            _ => panic!("unexpected {args:?}"),
        });
        assert_eq!(list_streams(&p4).unwrap().len(), 3);
        assert_eq!(
            list_files_with_digests(&p4, "//depot/main/", "@1234")
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            list_files_with_digests(&p4, "//depot/main", "")
                .unwrap()
                .len(),
            3
        );
        assert_eq!(list_shelf(&p4, 1234).unwrap().len(), 11);
        assert_eq!(
            p4.calls(),
            vec![
                args(&["-ztag", "streams", "-T", "Stream,Name,Parent,Type"]),
                args(&["-ztag", "fstat", "-Ol", "//depot/main/...@1234"]),
                args(&["-ztag", "fstat", "-Ol", "//depot/main/..."]),
                args(&["-ztag", "describe", "-S", "-s", "1234"]),
            ]
        );
    }

    #[test]
    fn print_reads_the_bytes_at_a_revision_changelist_or_shelf() {
        let p4 = FakeP4::new(|_: &[&str]| Ok("content".to_string()));
        for revision in ["", "#3", "@1234", "@=1234"] {
            assert_eq!(
                print_file(&p4, "//depot/main/a b.txt", revision),
                Ok(b"content".to_vec())
            );
        }
        assert_eq!(
            p4.calls(),
            vec![
                args(&["print", "-q", "//depot/main/a b.txt"]),
                args(&["print", "-q", "//depot/main/a b.txt#3"]),
                args(&["print", "-q", "//depot/main/a b.txt@1234"]),
                args(&["print", "-q", "//depot/main/a b.txt@=1234"]),
            ]
        );
    }

    #[test]
    fn operation_errors_name_the_command() {
        let p4 = FakeP4::new(|_: &[&str]| Err("Perforce client error: connect failed".to_string()));
        let err = list_shelf(&p4, 1234).unwrap_err();
        assert!(err.contains("p4 -ztag describe -S -s 1234"), "{err}");
        assert!(err.contains("connect failed"), "{err}");
        let err = print_file(&p4, "//depot/main/a.txt", "@=1234").unwrap_err();
        assert!(err.contains("print -q //depot/main/a.txt@=1234"), "{err}");

        let p4 = FakeP4::new(|_: &[&str]| Ok("... Name main\n".to_string()));
        let err = list_streams(&p4).unwrap_err();
        assert!(err.contains("p4 -ztag streams"), "{err}");
    }

    #[test]
    fn local_path_escapes_p4_revision_and_wildcard_characters() {
        assert_eq!(
            escape_local_path(r"C:\ws\a@b #1 50%*.txt"),
            r"C:\ws\a%40b %231 50%25%2A.txt"
        );
        assert_eq!(escape_local_path(r"C:\ws\plain.txt"), r"C:\ws\plain.txt");
    }
}
