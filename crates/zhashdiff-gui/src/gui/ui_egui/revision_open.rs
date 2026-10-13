use std::{
    env,
    path::{Path, PathBuf},
    process::{self, Child},
    sync::{
        Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread,
};

use zcommon::vcs::{git::GitCommand, p4::P4Command};
use zhashdiff::{
    external_diff_tool::{DiffPair, DiffTool, folder_labels, open_diff_tool},
    revision_pair::{PairSource, remove_temp_dir, tool_paths},
    vcs_source::ContentSource,
};

use crate::ui_egui::status_line::ToolMessage;

pub struct RevisionOpen {
    pub tool: DiffTool,
    pub exe: PathBuf,
    pub sides: [(ContentSource, PathBuf); 2],
    pub rel_path: String,
    pub warning: Option<String>,
}

static FETCHING: AtomicUsize = AtomicUsize::new(0);
static OPENED: AtomicU64 = AtomicU64::new(0);
static OUTCOME: Mutex<Option<ToolMessage>> = Mutex::new(None);

fn temp_root() -> PathBuf {
    env::temp_dir()
        .join("zhashdiff-revisions")
        .join(process::id().to_string())
}

pub fn is_revision(content: &ContentSource) -> bool {
    !matches!(content, ContentSource::Disk)
}

pub fn spawn(job: RevisionOpen) {
    let dir = temp_root().join(OPENED.fetch_add(1, Ordering::Relaxed).to_string());
    FETCHING.fetch_add(1, Ordering::Relaxed);
    thread::spawn(move || run(job, dir));
}

/// Read this before `take_outcome`: an outcome is stored before the count drops, so a frame that
/// saw no fetch in flight finds any result that is already there.
pub fn is_fetching() -> bool {
    FETCHING.load(Ordering::Relaxed) > 0
}

/// `Some(ToolMessage::default())` is a clean open: it replaces the "Fetching" status.
pub fn take_outcome() -> Option<ToolMessage> {
    OUTCOME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

/// The tools still running lose their files.
pub fn remove_all() {
    remove_temp_dir(&temp_root());
}

fn run(job: RevisionOpen, dir: PathBuf) {
    let (message, child) = match fetch_and_launch(&job, &dir) {
        Ok(child) => (
            ToolMessage {
                warning: job.warning,
                ..ToolMessage::default()
            },
            Some(child),
        ),
        Err(error) => {
            log::error!("{error}");
            (
                ToolMessage {
                    warning: job.warning,
                    error: Some(error),
                    ..ToolMessage::default()
                },
                None,
            )
        }
    };
    *OUTCOME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(message);
    FETCHING.fetch_sub(1, Ordering::Relaxed);

    if let Some(mut child) = child
        && let Err(err) = child.wait()
    {
        log::warn!("waiting for the diff tool: {err}");
    }
    remove_temp_dir(&dir);
}

fn fetch_and_launch(job: &RevisionOpen, dir: &Path) -> Result<Child, String> {
    let [(left_content, left_path), (right_content, right_path)] = &job.sides;
    let [left, right] = tool_paths(
        &GitCommand::default(),
        &P4Command::new(false),
        &job.tool,
        [
            PairSource {
                content: left_content,
                path: left_path,
            },
            PairSource {
                content: right_content,
                path: right_path,
            },
        ],
        &job.rel_path,
        dir,
    )
    .map_err(|err| format!("diffing failed: fetching {}: {err}", job.rel_path))?;
    let (left_name, right_name) = folder_labels(&job.rel_path);
    open_diff_tool(
        &job.exe,
        &job.tool,
        &DiffPair {
            left: &left,
            right: &right,
            left_name: &left_name,
            right_name: &right_name,
        },
    )
    .map_err(|err| {
        // The spawn error doesn't name the program.
        format!(
            "diffing failed: {} ({}): {err}",
            job.tool.name,
            job.exe.display()
        )
    })
}
