use std::{
    env,
    path::{Path, PathBuf},
    process::{self, Child},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use zcommon::{
    job::{Job, JobCtx},
    vcs::{git::GitCommand, p4::P4Command},
};
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

struct Open {
    job: Job<ToolMessage>,
    delivered: bool,
}

static OPENED: AtomicU64 = AtomicU64::new(0);
static OPENS: Mutex<Vec<Open>> = Mutex::new(Vec::new());

fn opens() -> std::sync::MutexGuard<'static, Vec<Open>> {
    OPENS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
    let mut open = Job::new(|| {});
    open.start(move |ctx| run(job, dir, ctx));
    opens().push(Open {
        job: open,
        delivered: false,
    });
}

/// Read this before `take_outcome`: a fetch counts until its outcome is taken, so a frame that
/// saw no fetch in flight has already taken every result.
pub fn is_fetching() -> bool {
    opens().iter().any(|open| !open.delivered)
}

/// `Some(ToolMessage::default())` is a clean open: it replaces the "Fetching" status.
pub fn take_outcome() -> Option<ToolMessage> {
    let mut opens = opens();
    let mut outcome = None;
    for open in opens.iter_mut() {
        if let Some(message) = open.job.poll().pop() {
            open.delivered = true;
            outcome = Some(message);
        }
    }
    opens.retain(|open| !open.delivered || open.job.is_running());
    outcome
}

/// The tools still running lose their files.
pub fn remove_all() {
    remove_temp_dir(&temp_root());
}

fn run(job: RevisionOpen, dir: PathBuf, ctx: &JobCtx<ToolMessage>) {
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
    ctx.send(message);

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
