//! Project file watcher (task T7.1).
//!
//! Watches the roots of projects DevPulse currently sees running, and nothing
//! else. A developer machine has thousands of directories; watching the ones
//! with a live service in them is the whole point of having discovered them.
//!
//! Three rules keep the cost honest (`AGENTS.md` rule 7):
//!
//! * only active project roots are watched, and never more than
//!   [`MAX_WATCHED_ROOTS`] of them;
//! * build output and VCS internals are ignored, because `node_modules` and
//!   `target` produce far more events than source files and answer no question;
//! * changes are coalesced per project over [`COALESCE_WINDOW`], so a save that
//!   rewrites forty files is one change, not forty.
//!
//! Paths are reported, never contents: DevPulse does not read the developer's
//! files.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use notify::event::{EventKind as NotifyKind, ModifyKind};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Upper bound on watched roots. A developer with more than this many projects
/// running at once is served by the most recently active ones.
pub const MAX_WATCHED_ROOTS: usize = 32;

/// Changes to one project inside this window are reported once.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(500);

/// Queue depth for the change channel. A full queue drops changes rather than
/// growing: one missed "something changed" is recoverable, unbounded memory is
/// not.
const QUEUE_DEPTH: usize = 256;

/// Directory names never worth watching. Matching is on a whole path
/// component, so a source directory called `distribution` is not caught by
/// `dist`.
const IGNORED_COMPONENTS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    ".next",
    ".nuxt",
    ".turbo",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".gradle",
    ".idea",
    ".vscode",
    "vendor",
    "coverage",
    ".DS_Store",
];

/// File suffixes that are noise: editor swap files and write-then-rename
/// temporaries.
const IGNORED_SUFFIXES: &[&str] = &[".swp", ".swx", "~", ".tmp", ".lock"];

/// Substrings that mark a write-then-rename temporary, whatever it ends with.
/// Editors and tools produce names like `README.md.tmp.13597.e444` — the real
/// change arrives as the rename, so reporting the temporary is double-counting.
const IGNORED_INFIXES: &[&str] = &[".tmp.", ".swap.", "~$"];

/// One coalesced change inside a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// The watched project root the change happened under.
    pub root: PathBuf,
    /// The path that changed. Reported for display and correlation only; its
    /// contents are never read.
    pub path: PathBuf,
    pub at: SystemTime,
}

/// Whether a path is worth reporting.
///
/// Public because it is the rule the daemon's behaviour depends on, and a rule
/// nobody can test is a rule nobody can trust.
pub fn is_interesting(path: &Path) -> bool {
    let ignored_component = path.components().any(|component| match component {
        Component::Normal(name) => name
            .to_str()
            .is_some_and(|name| IGNORED_COMPONENTS.contains(&name)),
        _ => false,
    });
    if ignored_component {
        return false;
    }

    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    !IGNORED_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
        && !IGNORED_INFIXES.iter().any(|infix| name.contains(infix))
}

/// Whether a notify event is a content change rather than metadata noise.
fn is_content_change(kind: &NotifyKind) -> bool {
    match kind {
        NotifyKind::Create(_) | NotifyKind::Remove(_) => true,
        // A `touch` or a permission change is not a code change.
        NotifyKind::Modify(ModifyKind::Metadata(_)) => false,
        NotifyKind::Modify(_) => true,
        NotifyKind::Access(_) | NotifyKind::Other | NotifyKind::Any => false,
    }
}

/// Watches project roots and reports coalesced changes.
pub struct ProjectWatcher {
    watcher: RecommendedWatcher,
    /// Roots currently watched, in insertion order so the oldest is the one
    /// dropped when the cap is reached.
    watched: Vec<PathBuf>,
    receiver: mpsc::Receiver<FileChange>,
}

impl ProjectWatcher {
    /// Start a watcher. The returned watcher watches nothing until
    /// [`ProjectWatcher::sync_roots`] is called.
    pub fn new() -> Result<Self, notify::Error> {
        let (tx, receiver) = mpsc::channel(QUEUE_DEPTH);
        let mut last_reported: HashMap<PathBuf, SystemTime> = HashMap::new();

        let watcher = RecommendedWatcher::new(
            move |event: Result<notify::Event, notify::Error>| {
                let event = match event {
                    Ok(event) => event,
                    Err(error) => {
                        debug!(%error, "file watch error");
                        return;
                    }
                };
                if !is_content_change(&event.kind) {
                    return;
                }

                let at = SystemTime::now();
                for path in event.paths.iter().filter(|path| is_interesting(path)) {
                    // Coalescing is keyed on the *watched root* the path is
                    // under, which the receiver resolves; here the key is the
                    // path's parent, which is close enough to collapse a burst
                    // from one save without collapsing two different projects.
                    let key = path.parent().unwrap_or(path).to_path_buf();
                    let recent = last_reported
                        .get(&key)
                        .and_then(|last| at.duration_since(*last).ok())
                        .is_some_and(|since| since < COALESCE_WINDOW);
                    if recent {
                        continue;
                    }
                    last_reported.insert(key, at);

                    if tx
                        .try_send(FileChange {
                            // Filled in by `changes()`, which knows the roots.
                            root: PathBuf::new(),
                            path: path.clone(),
                            at,
                        })
                        .is_err()
                    {
                        debug!("file change queue is full; dropping a change");
                    }
                }
            },
            notify::Config::default(),
        )?;

        Ok(Self {
            watcher,
            watched: Vec::new(),
            receiver,
        })
    }

    /// Watch exactly `roots`, adding what is new and dropping what is gone.
    ///
    /// Roots that cannot be watched (deleted, or on a filesystem the platform
    /// cannot watch) are skipped with a log line rather than failing the sync:
    /// a project that disappeared is normal.
    pub fn sync_roots(&mut self, roots: &[PathBuf]) {
        let keep: Vec<PathBuf> = roots.iter().take(MAX_WATCHED_ROOTS).cloned().collect();

        for root in self.watched.clone() {
            if !keep.contains(&root) {
                if let Err(error) = self.watcher.unwatch(&root) {
                    debug!(%error, root = %root.display(), "unwatch failed");
                }
                self.watched.retain(|watched| watched != &root);
            }
        }

        for root in keep {
            if self.watched.contains(&root) || !root.is_dir() {
                continue;
            }
            match self.watcher.watch(&root, RecursiveMode::Recursive) {
                Ok(()) => {
                    debug!(root = %root.display(), "watching project root");
                    self.watched.push(root);
                }
                Err(error) => warn!(%error, root = %root.display(), "cannot watch project root"),
            }
        }
    }

    /// Roots currently watched.
    pub fn watched(&self) -> &[PathBuf] {
        &self.watched
    }

    /// Take every change queued so far, up to `max`, without waiting.
    ///
    /// The daemon drains once per tick: a change is interesting because of
    /// what happened around it, and "around it" is measured in ticks.
    pub fn drain(&mut self, max: usize) -> Vec<FileChange> {
        let mut changes = Vec::new();
        while changes.len() < max {
            match self.receiver.try_recv() {
                Ok(change) => {
                    if let Some(change) = self.attribute(change) {
                        changes.push(change);
                    }
                }
                Err(_) => break,
            }
        }
        changes
    }

    /// Resolve which watched root a change happened under. `None` when the
    /// root was unwatched between the event and the drain — attributing it to
    /// some other project would be worse than dropping it.
    fn attribute(&self, mut change: FileChange) -> Option<FileChange> {
        let root = self
            .watched
            .iter()
            .filter(|root| change.path.starts_with(root))
            // The deepest matching root wins, so a project nested inside
            // another is attributed to itself.
            .max_by_key(|root| root.components().count())?;
        change.root = root.clone();
        Some(change)
    }

    /// Receive the next change, with its project root resolved.
    ///
    /// `None` means the watcher has been dropped.
    pub async fn next_change(&mut self) -> Option<FileChange> {
        loop {
            let change = self.receiver.recv().await?;
            if let Some(change) = self.attribute(change) {
                return Some(change);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_output_and_vcs_internals_are_ignored() {
        for path in [
            "/tmp/app/node_modules/react/index.js",
            "/tmp/app/.git/index",
            "/tmp/app/target/debug/build.rs",
            "/tmp/app/.next/cache/x",
            "/tmp/app/__pycache__/mod.pyc",
            "/tmp/app/src/main.rs.swp",
            "/tmp/app/Cargo.lock",
            "/tmp/app/README.md.tmp.13597.e4443026deec",
        ] {
            assert!(!is_interesting(Path::new(path)), "{path} must be ignored");
        }
    }

    #[test]
    fn source_files_are_interesting() {
        for path in [
            "/tmp/app/src/main.rs",
            "/tmp/app/package.json",
            "/tmp/app/distribution/index.ts",
            "/tmp/app/lib/target_practice.py",
        ] {
            assert!(is_interesting(Path::new(path)), "{path} must be reported");
        }
    }

    #[test]
    fn metadata_changes_are_not_code_changes() {
        use notify::event::{AccessKind, CreateKind, MetadataKind};

        assert!(is_content_change(&NotifyKind::Create(CreateKind::File)));
        assert!(is_content_change(&NotifyKind::Modify(ModifyKind::Data(
            notify::event::DataChange::Content
        ))));
        assert!(!is_content_change(&NotifyKind::Modify(
            ModifyKind::Metadata(MetadataKind::Permissions)
        )));
        assert!(!is_content_change(&NotifyKind::Access(AccessKind::Read)));
    }

    #[tokio::test]
    async fn syncing_roots_adds_and_drops_watches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(&a).expect("mkdir a");
        std::fs::create_dir_all(&b).expect("mkdir b");

        let mut watcher = ProjectWatcher::new().expect("watcher starts");
        watcher.sync_roots(&[a.clone(), b.clone()]);
        assert_eq!(watcher.watched().len(), 2);

        watcher.sync_roots(std::slice::from_ref(&a));
        assert_eq!(watcher.watched(), std::slice::from_ref(&a));

        // A root that does not exist is skipped, not an error.
        watcher.sync_roots(&[a.clone(), dir.path().join("gone")]);
        assert_eq!(watcher.watched(), &[a]);
    }

    #[tokio::test]
    async fn a_saved_file_is_reported_under_its_project() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical root");
        let mut watcher = ProjectWatcher::new().expect("watcher starts");
        watcher.sync_roots(std::slice::from_ref(&root));

        // Give the platform watcher a moment to arm before writing.
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::fs::write(root.join("main.rs"), "fn main() {}").expect("write");

        let change = tokio::time::timeout(Duration::from_secs(10), watcher.next_change())
            .await
            .expect("a change arrives")
            .expect("the watcher is alive");

        assert_eq!(change.root, root);
        assert!(change.path.ends_with("main.rs"), "{:?}", change.path);
    }
}
