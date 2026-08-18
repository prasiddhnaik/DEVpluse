//! Project root resolution (task T0.5).
//!
//! Input: a process working directory.
//! Output: the nearest defensible project root, the evidence that produced it,
//! and a confidence value.
//!
//! # Policy
//!
//! Candidate roots are collected by walking `cwd` and every ancestor, recording
//! which markers each directory holds. Selection is then deterministic:
//!
//! 1. **nearest `.git`** — a repository boundary is the strongest signal we can
//!    observe locally. Nested repositories (vendored checkouts, submodules) are
//!    therefore separate projects.
//! 2. **outermost workspace root** — `pnpm-workspace.yaml`, a Cargo
//!    `[workspace]`, or a `package.json` with `workspaces`. A monorepo without
//!    Git collapses to one project rather than one project per package.
//! 3. **nearest package manifest** — `Cargo.toml`, `package.json`,
//!    `pyproject.toml`.
//! 4. **nearest compose file** — `compose.yml` / `docker-compose.yml`.
//!
//! Directories that are never projects (`/`, `$HOME` and its ancestors, common
//! system prefixes) are excluded even when they hold markers, because a stray
//! `~/package.json` would otherwise swallow every Node process on the machine.
//!
//! Nothing here silently collapses a weak match into a strong one: the caller
//! always receives the confidence and the evidence list.

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// A file or directory whose presence suggests a project root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMarker {
    /// `.git` directory, or `.git` file for a linked worktree.
    Git,
    /// `Cargo.toml` containing a `[workspace]` table.
    CargoWorkspace,
    /// `Cargo.toml` without a `[workspace]` table.
    CargoPackage,
    /// `package.json` containing a `workspaces` key.
    NodeWorkspace,
    /// `package.json` without a `workspaces` key.
    NodePackage,
    /// `pnpm-workspace.yaml` / `pnpm-workspace.yml`.
    PnpmWorkspace,
    /// `pyproject.toml`.
    PythonProject,
    /// `compose.yml`, `compose.yaml`, `docker-compose.yml`, `docker-compose.yaml`.
    ComposeFile,
}

impl ProjectMarker {
    /// True when the marker denotes a multi-package workspace root.
    pub fn is_workspace(self) -> bool {
        matches!(
            self,
            Self::CargoWorkspace | Self::NodeWorkspace | Self::PnpmWorkspace
        )
    }

    /// True when the marker denotes a single buildable package.
    pub fn is_package(self) -> bool {
        matches!(
            self,
            Self::CargoPackage | Self::NodePackage | Self::PythonProject
        )
    }
}

impl fmt::Display for ProjectMarker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Git => "git",
            Self::CargoWorkspace => "cargo-workspace",
            Self::CargoPackage => "cargo-package",
            Self::NodeWorkspace => "node-workspace",
            Self::NodePackage => "node-package",
            Self::PnpmWorkspace => "pnpm-workspace",
            Self::PythonProject => "pyproject",
            Self::ComposeFile => "compose",
        };
        f.write_str(s)
    }
}

/// A marker observed at a concrete path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkerHit {
    pub marker: ProjectMarker,
    pub path: PathBuf,
}

/// Why a root was chosen, and what was seen there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProjectEvidence {
    /// Nearest Git repository boundary above the working directory.
    GitRoot { path: PathBuf },
    /// Outermost multi-package workspace root.
    WorkspaceRoot {
        path: PathBuf,
        marker: ProjectMarker,
    },
    /// Nearest single-package manifest.
    ProjectManifest {
        path: PathBuf,
        marker: ProjectMarker,
    },
    /// Compose file found at the root.
    ComposeFile { path: PathBuf },
    /// The working directory sits `depth` levels below the chosen root.
    CwdAncestry { cwd: PathBuf, depth: usize },
}

/// The class of root that was selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    GitRepository,
    Workspace,
    Package,
    ComposeStack,
}

impl fmt::Display for RootKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::GitRepository => "git-repository",
            Self::Workspace => "workspace",
            Self::Package => "package",
            Self::ComposeStack => "compose-stack",
        };
        f.write_str(s)
    }
}

/// A resolved project root plus the reasoning behind it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectMatch {
    /// Canonicalised project root.
    pub root: PathBuf,
    /// Directory name of the root; a display hint only, never an identity.
    pub name: String,
    pub kind: RootKind,
    /// `0.0..=1.0`. See module docs for the policy behind each value.
    pub confidence: f32,
    pub evidence: Vec<ProjectEvidence>,
    /// Every marker found at the chosen root.
    pub markers: Vec<MarkerHit>,
}

/// Why no project could be resolved. Callers must not treat these as errors to
/// paper over — an unresolved process is a legitimate outcome.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NoProject {
    /// The working directory could not be read (process exited, permission
    /// denied, or the directory was deleted).
    #[error("working directory unavailable: {path} ({reason})")]
    PathUnavailable { path: PathBuf, reason: String },
    /// Markers were found, but only in directories that can never be projects.
    #[error("only excluded directories matched (nearest: {path})")]
    ExcludedRootOnly { path: PathBuf },
    /// The directory is not inside anything that looks like a project.
    #[error("no project markers found above {path}")]
    NoMarkers { path: PathBuf },
}

/// Tunables for [`ProjectResolver`]. Tests construct this explicitly so that
/// resolution never depends on the developer's environment.
#[derive(Debug, Clone)]
pub struct ResolverConfig {
    /// Directories that must never be reported as a project root.
    pub excluded_roots: BTreeSet<PathBuf>,
    /// Path components that mark vendored dependency trees. A directory with
    /// any of these in its path is never a project root: the `package.json`
    /// under `node_modules/` describes a dependency, not something the
    /// developer is running as their project.
    pub excluded_components: BTreeSet<String>,
    /// Maximum number of ancestors inspected, including the directory itself.
    pub max_depth: usize,
}

impl ResolverConfig {
    /// Config with no exclusions. Intended for tests and for callers that
    /// supply their own policy.
    pub fn bare() -> Self {
        Self {
            excluded_roots: BTreeSet::new(),
            excluded_components: BTreeSet::new(),
            max_depth: 64,
        }
    }

    /// Config derived from the current machine: `$HOME`, every ancestor of
    /// `$HOME`, and common system prefixes are excluded.
    pub fn detect() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::for_home(home.as_deref())
    }

    /// Config for an explicit home directory. Exposed so tests can exercise the
    /// exclusion policy deterministically.
    pub fn for_home(home: Option<&Path>) -> Self {
        let mut excluded_roots: BTreeSet<PathBuf> = [
            "/",
            "/usr",
            "/usr/local",
            "/usr/share",
            "/opt",
            "/etc",
            "/var",
            "/private/var",
            "/private/tmp",
            "/tmp",
            "/Users",
            "/home",
            "/Applications",
            "/Library",
            "/System/Volumes/Data",
        ]
        .iter()
        .map(PathBuf::from)
        .collect();

        if let Some(home) = home {
            for ancestor in home.ancestors() {
                excluded_roots.insert(ancestor.to_path_buf());
            }
        }

        Self {
            excluded_roots,
            excluded_components: ["node_modules", "site-packages", ".venv", "vendor", "Pods"]
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            max_depth: 64,
        }
    }

    fn is_excluded(&self, dir: &Path) -> bool {
        if self.excluded_roots.contains(dir) {
            return true;
        }
        dir.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| self.excluded_components.contains(name))
        })
    }
}

impl Default for ResolverConfig {
    fn default() -> Self {
        Self::detect()
    }
}

/// How long a cwd → root answer is reused. Long enough that a 1 Hz snapshot
/// loop does not re-stat the same tree every tick; short enough that `git init`
/// in a new directory still shows up within a few seconds.
const RESOLVE_CACHE_TTL: Duration = Duration::from_secs(5);
const RESOLVE_CACHE_MAX: usize = 512;

#[derive(Clone, Debug)]
struct CachedResolve {
    result: Result<ProjectMatch, NoProject>,
    stored_at: Instant,
}

/// Resolves working directories to project roots.
///
/// Clones share the cwd cache: the snapshot loop resolves hundreds of process
/// working directories per tick, and most of them have not moved.
#[derive(Debug, Clone)]
pub struct ProjectResolver {
    config: ResolverConfig,
    cache: Arc<Mutex<HashMap<PathBuf, CachedResolve>>>,
}

impl Default for ProjectResolver {
    fn default() -> Self {
        Self::new(ResolverConfig::default())
    }
}

#[derive(Debug)]
struct Candidate {
    dir: PathBuf,
    markers: Vec<MarkerHit>,
    excluded: bool,
}

impl ProjectResolver {
    pub fn new(config: ResolverConfig) -> Self {
        Self {
            config,
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn config(&self) -> &ResolverConfig {
        &self.config
    }

    /// Resolve a process working directory to a project root.
    pub fn resolve(&self, cwd: &Path) -> Result<ProjectMatch, NoProject> {
        let cwd = std::fs::canonicalize(cwd).map_err(|err| NoProject::PathUnavailable {
            path: cwd.to_path_buf(),
            reason: err.to_string(),
        })?;

        if let Some(hit) = self.cache_get(&cwd) {
            return hit;
        }

        let result = self.resolve_uncached(&cwd);
        // A path that could not be read is often a process that just exited;
        // caching that would hide the next process that reuses the directory.
        if !matches!(result, Err(NoProject::PathUnavailable { .. })) {
            self.cache_put(cwd.clone(), result.clone());
        }
        result
    }

    fn cache_get(&self, cwd: &Path) -> Option<Result<ProjectMatch, NoProject>> {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let entry = cache.get(cwd)?;
        if entry.stored_at.elapsed() > RESOLVE_CACHE_TTL {
            cache.remove(cwd);
            return None;
        }
        Some(entry.result.clone())
    }

    fn cache_put(&self, cwd: PathBuf, result: Result<ProjectMatch, NoProject>) {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.len() >= RESOLVE_CACHE_MAX {
            cache.retain(|_, entry| entry.stored_at.elapsed() <= RESOLVE_CACHE_TTL);
            if cache.len() >= RESOLVE_CACHE_MAX {
                cache.clear();
            }
        }
        cache.insert(
            cwd,
            CachedResolve {
                result,
                stored_at: Instant::now(),
            },
        );
    }

    fn resolve_uncached(&self, cwd: &Path) -> Result<ProjectMatch, NoProject> {
        let candidates: Vec<Candidate> = cwd
            .ancestors()
            .take(self.config.max_depth)
            .filter_map(|dir| {
                // Excluded roots (home, /Applications, /usr, …) are never
                // projects. Stat-ing them for markers is wasted work; keep
                // walking so a project sitting *under* an excluded ancestor
                // (anything inside $HOME) can still match.
                if self.config.is_excluded(dir) {
                    return Some(Candidate {
                        dir: dir.to_path_buf(),
                        markers: Vec::new(),
                        excluded: true,
                    });
                }
                let markers = scan_directory(dir);
                if markers.is_empty() {
                    return None;
                }
                Some(Candidate {
                    dir: dir.to_path_buf(),
                    markers,
                    excluded: false,
                })
            })
            .collect();

        if candidates.is_empty() {
            return Err(NoProject::NoMarkers {
                path: cwd.to_path_buf(),
            });
        }

        let usable: Vec<&Candidate> = candidates.iter().filter(|c| !c.excluded).collect();
        if usable.is_empty() {
            return Err(NoProject::ExcludedRootOnly {
                path: candidates[0].dir.clone(),
            });
        }

        let Some((candidate, kind)) = select_root(&usable) else {
            return Err(NoProject::NoMarkers {
                path: cwd.to_path_buf(),
            });
        };

        Ok(build_match(cwd, candidate, kind))
    }
}

/// Ordered selection over usable candidates (nearest first).
fn select_root<'a>(usable: &[&'a Candidate]) -> Option<(&'a Candidate, RootKind)> {
    if let Some(c) = usable.iter().find(|c| has(c, ProjectMarker::Git)) {
        return Some((c, RootKind::GitRepository));
    }
    // Outermost workspace: a monorepo is one project, not one per package.
    if let Some(c) = usable.iter().rev().find(|c| has_any_workspace(c)) {
        return Some((c, RootKind::Workspace));
    }
    if let Some(c) = usable.iter().find(|c| has_any_package(c)) {
        return Some((c, RootKind::Package));
    }
    if let Some(c) = usable.iter().find(|c| has(c, ProjectMarker::ComposeFile)) {
        return Some((c, RootKind::ComposeStack));
    }
    None
}

fn build_match(cwd: &Path, candidate: &Candidate, kind: RootKind) -> ProjectMatch {
    let root = candidate.dir.clone();
    let depth = cwd
        .strip_prefix(&root)
        .map(|rest| rest.components().count())
        .unwrap_or(0);

    let mut evidence = Vec::with_capacity(candidate.markers.len() + 1);
    for hit in &candidate.markers {
        evidence.push(match hit.marker {
            ProjectMarker::Git => ProjectEvidence::GitRoot {
                path: hit.path.clone(),
            },
            ProjectMarker::ComposeFile => ProjectEvidence::ComposeFile {
                path: hit.path.clone(),
            },
            m if m.is_workspace() => ProjectEvidence::WorkspaceRoot {
                path: hit.path.clone(),
                marker: m,
            },
            m => ProjectEvidence::ProjectManifest {
                path: hit.path.clone(),
                marker: m,
            },
        });
    }
    evidence.push(ProjectEvidence::CwdAncestry {
        cwd: cwd.to_path_buf(),
        depth,
    });

    let corroborated = candidate
        .markers
        .iter()
        .any(|h| h.marker != ProjectMarker::Git);
    let confidence = match kind {
        RootKind::GitRepository if corroborated => 0.95,
        RootKind::GitRepository => 0.90,
        RootKind::Workspace => 0.85,
        RootKind::Package => 0.75,
        RootKind::ComposeStack => 0.70,
    };

    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned());

    ProjectMatch {
        root,
        name,
        kind,
        confidence,
        evidence,
        markers: candidate.markers.clone(),
    }
}

fn has(candidate: &Candidate, marker: ProjectMarker) -> bool {
    candidate.markers.iter().any(|h| h.marker == marker)
}

fn has_any_workspace(candidate: &Candidate) -> bool {
    candidate.markers.iter().any(|h| h.marker.is_workspace())
}

fn has_any_package(candidate: &Candidate) -> bool {
    candidate.markers.iter().any(|h| h.marker.is_package())
}

/// Inspect a single directory for project markers. Purely stat-based apart from
/// two small manifest reads used to distinguish workspace roots from packages.
fn scan_directory(dir: &Path) -> Vec<MarkerHit> {
    let mut hits = Vec::new();

    let git = dir.join(".git");
    if git.exists() {
        hits.push(MarkerHit {
            marker: ProjectMarker::Git,
            path: git,
        });
    }

    let cargo = dir.join("Cargo.toml");
    if cargo.is_file() {
        let marker = if cargo_declares_workspace(&cargo) {
            ProjectMarker::CargoWorkspace
        } else {
            ProjectMarker::CargoPackage
        };
        hits.push(MarkerHit {
            marker,
            path: cargo,
        });
    }

    let package_json = dir.join("package.json");
    if package_json.is_file() {
        let marker = if package_json_declares_workspaces(&package_json) {
            ProjectMarker::NodeWorkspace
        } else {
            ProjectMarker::NodePackage
        };
        hits.push(MarkerHit {
            marker,
            path: package_json,
        });
    }

    for name in ["pnpm-workspace.yaml", "pnpm-workspace.yml"] {
        let path = dir.join(name);
        if path.is_file() {
            hits.push(MarkerHit {
                marker: ProjectMarker::PnpmWorkspace,
                path,
            });
            break;
        }
    }

    let pyproject = dir.join("pyproject.toml");
    if pyproject.is_file() {
        hits.push(MarkerHit {
            marker: ProjectMarker::PythonProject,
            path: pyproject,
        });
    }

    for name in [
        "compose.yml",
        "compose.yaml",
        "docker-compose.yml",
        "docker-compose.yaml",
    ] {
        let path = dir.join(name);
        if path.is_file() {
            hits.push(MarkerHit {
                marker: ProjectMarker::ComposeFile,
                path,
            });
            break;
        }
    }

    hits
}

/// Detect a Cargo workspace without pulling in a TOML parser: a `[workspace]`
/// or `[workspace.*]` table header always starts its own line.
fn cargo_declares_workspace(path: &Path) -> bool {
    let Ok(contents) = read_manifest(path) else {
        return false;
    };
    contents.lines().any(|line| {
        let line = line.trim();
        line == "[workspace]" || line.starts_with("[workspace.")
    })
}

fn package_json_declares_workspaces(path: &Path) -> bool {
    let Ok(contents) = read_manifest(path) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&contents)
        .ok()
        .and_then(|v| v.get("workspaces").cloned())
        .is_some()
}

/// Manifests are small; refuse to read anything unreasonable.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

fn read_manifest(path: &Path) -> std::io::Result<String> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        tracing::debug!(path = %path.display(), size = metadata.len(), "manifest too large to inspect");
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "manifest too large",
        ));
    }
    std::fs::read_to_string(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture {
        dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = fs::canonicalize(dir.path()).expect("canonicalize tempdir");
            Self { dir, root }
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }

        fn mkdir(&self, rel: &str) -> PathBuf {
            let path = self.path(rel);
            fs::create_dir_all(&path).expect("create dir");
            path
        }

        fn write(&self, rel: &str, contents: &str) -> PathBuf {
            let path = self.path(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(&path, contents).expect("write file");
            path
        }

        /// `.git` as a directory, like a normal clone.
        fn git(&self, rel: &str) -> PathBuf {
            self.mkdir(&format!("{rel}/.git"))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // TempDir cleans itself up; keep the handle alive explicitly.
            let _ = &self.dir;
        }
    }

    fn resolver() -> ProjectResolver {
        ProjectResolver::new(ResolverConfig::bare())
    }

    #[test]
    fn resolves_node_app_by_package_manifest() {
        let fx = Fixture::new();
        fx.write("app/package.json", r#"{"name":"app"}"#);
        let cwd = fx.mkdir("app/src");

        let m = resolver().resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("app"));
        assert_eq!(m.kind, RootKind::Package);
        assert_eq!(m.confidence, 0.75);
    }

    #[test]
    fn resolves_rust_app_by_cargo_manifest() {
        let fx = Fixture::new();
        fx.write("api/Cargo.toml", "[package]\nname = \"api\"\n");
        let cwd = fx.mkdir("api/src");

        let m = resolver().resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("api"));
        assert_eq!(m.kind, RootKind::Package);
        assert!(
            m.markers
                .iter()
                .any(|h| h.marker == ProjectMarker::CargoPackage)
        );
    }

    #[test]
    fn resolves_python_app_by_pyproject() {
        let fx = Fixture::new();
        fx.write("svc/pyproject.toml", "[project]\nname = \"svc\"\n");
        let cwd = fx.mkdir("svc/svc");

        let m = resolver().resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("svc"));
        assert_eq!(m.kind, RootKind::Package);
    }

    #[test]
    fn git_root_beats_inner_package_manifest() {
        let fx = Fixture::new();
        fx.git("repo");
        fx.write("repo/package.json", r#"{"name":"root"}"#);
        fx.write("repo/apps/web/package.json", r#"{"name":"web"}"#);
        let cwd = fx.mkdir("repo/apps/web/src");

        let m = resolver().resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("repo"));
        assert_eq!(m.kind, RootKind::GitRepository);
        assert_eq!(
            m.confidence, 0.95,
            "git plus manifest is fully corroborated"
        );
        assert!(matches!(
            m.evidence.last(),
            Some(ProjectEvidence::CwdAncestry { depth: 3, .. })
        ));
    }

    #[test]
    fn bare_git_root_has_lower_confidence_than_corroborated_root() {
        let fx = Fixture::new();
        fx.git("repo");
        let cwd = fx.mkdir("repo/src");

        let m = resolver().resolve(&cwd).expect("match");
        assert_eq!(m.kind, RootKind::GitRepository);
        assert_eq!(m.confidence, 0.90);
    }

    #[test]
    fn nested_git_repository_is_its_own_project() {
        let fx = Fixture::new();
        fx.git("outer");
        fx.write("outer/Cargo.toml", "[workspace]\nmembers = []\n");
        fx.git("outer/vendor/inner");
        fx.write("outer/vendor/inner/package.json", r#"{"name":"inner"}"#);
        let cwd = fx.mkdir("outer/vendor/inner/src");

        let m = resolver().resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("outer/vendor/inner"));
    }

    #[test]
    fn git_worktree_file_counts_as_repository_root() {
        let fx = Fixture::new();
        fx.write("wt/.git", "gitdir: /elsewhere/.git/worktrees/wt\n");
        let cwd = fx.mkdir("wt/src");

        let m = resolver().resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("wt"));
        assert_eq!(m.kind, RootKind::GitRepository);
    }

    #[test]
    fn monorepo_without_git_collapses_to_outermost_workspace() {
        let fx = Fixture::new();
        fx.write("mono/pnpm-workspace.yaml", "packages:\n  - apps/*\n");
        fx.write(
            "mono/package.json",
            r#"{"name":"mono","workspaces":["apps/*"]}"#,
        );
        fx.write("mono/apps/web/package.json", r#"{"name":"web"}"#);
        let cwd = fx.mkdir("mono/apps/web/src");

        let m = resolver().resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("mono"));
        assert_eq!(m.kind, RootKind::Workspace);
        assert_eq!(m.confidence, 0.85);
    }

    #[test]
    fn cargo_workspace_beats_inner_crate() {
        let fx = Fixture::new();
        fx.write("ws/Cargo.toml", "[workspace]\nmembers = [\"crates/*\"]\n");
        fx.write("ws/crates/api/Cargo.toml", "[package]\nname = \"api\"\n");
        let cwd = fx.mkdir("ws/crates/api/src");

        let m = resolver().resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("ws"));
        assert_eq!(m.kind, RootKind::Workspace);
    }

    #[test]
    fn compose_only_directory_is_a_low_confidence_stack() {
        let fx = Fixture::new();
        fx.write("stack/docker-compose.yml", "services: {}\n");
        let cwd = fx.mkdir("stack/data");

        let m = resolver().resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("stack"));
        assert_eq!(m.kind, RootKind::ComposeStack);
        assert_eq!(m.confidence, 0.70);
    }

    #[test]
    fn unrelated_system_directory_has_no_project() {
        let fx = Fixture::new();
        let cwd = fx.mkdir("var/log/somewhere");

        let err = resolver().resolve(&cwd).expect_err("no project");
        assert!(matches!(err, NoProject::NoMarkers { .. }), "got {err:?}");
    }

    #[test]
    fn markers_in_home_directory_are_rejected() {
        let fx = Fixture::new();
        let home = fx.mkdir("Users/dev");
        fx.write("Users/dev/package.json", r#"{"name":"dotfiles"}"#);
        let cwd = fx.mkdir("Users/dev/scratch");

        let resolver = ProjectResolver::new(ResolverConfig::for_home(Some(&home)));
        let err = resolver.resolve(&cwd).expect_err("home is not a project");
        assert_eq!(err, NoProject::ExcludedRootOnly { path: home });
    }

    #[test]
    fn project_below_home_still_resolves() {
        let fx = Fixture::new();
        let home = fx.mkdir("Users/dev");
        fx.write("Users/dev/package.json", r#"{"name":"dotfiles"}"#);
        fx.git("Users/dev/code/app");
        let cwd = fx.mkdir("Users/dev/code/app/src");

        let resolver = ProjectResolver::new(ResolverConfig::for_home(Some(&home)));
        let m = resolver.resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("Users/dev/code/app"));
    }

    #[test]
    fn dependency_inside_node_modules_is_not_a_project() {
        let fx = Fixture::new();
        let home = fx.mkdir("Users/dev");
        fx.write(
            "Users/dev/node_modules/@scope/tool/package.json",
            r#"{"name":"tool"}"#,
        );
        let cwd = fx.mkdir("Users/dev/node_modules/@scope/tool/bin");

        let resolver = ProjectResolver::new(ResolverConfig::for_home(Some(&home)));
        let err = resolver
            .resolve(&cwd)
            .expect_err("a dependency is not a project");
        assert!(
            matches!(err, NoProject::ExcludedRootOnly { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn process_running_inside_a_dependency_resolves_to_the_enclosing_project() {
        let fx = Fixture::new();
        let home = fx.mkdir("Users/dev");
        fx.git("Users/dev/code/app");
        fx.write("Users/dev/code/app/package.json", r#"{"name":"app"}"#);
        fx.write(
            "Users/dev/code/app/node_modules/esbuild/package.json",
            r#"{"name":"esbuild"}"#,
        );
        let cwd = fx.mkdir("Users/dev/code/app/node_modules/esbuild/bin");

        let resolver = ProjectResolver::new(ResolverConfig::for_home(Some(&home)));
        let m = resolver.resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("Users/dev/code/app"));
        assert_eq!(m.kind, RootKind::GitRepository);
    }

    #[test]
    fn a_project_containing_node_modules_still_resolves() {
        let fx = Fixture::new();
        let home = fx.mkdir("Users/dev");
        let cwd = fx.mkdir("Users/dev/code/app");
        fx.write("Users/dev/code/app/package.json", r#"{"name":"app"}"#);
        fx.write(
            "Users/dev/code/app/node_modules/left-pad/package.json",
            r#"{"name":"left-pad"}"#,
        );

        let resolver = ProjectResolver::new(ResolverConfig::for_home(Some(&home)));
        let m = resolver.resolve(&cwd).expect("match");
        assert_eq!(m.root, fx.path("Users/dev/code/app"));
        assert_eq!(m.kind, RootKind::Package);
    }

    #[test]
    fn missing_directory_reports_path_unavailable() {
        let fx = Fixture::new();
        let cwd = fx.path("does/not/exist");

        let err = resolver().resolve(&cwd).expect_err("missing path");
        assert!(
            matches!(err, NoProject::PathUnavailable { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn resolution_is_deterministic() {
        let fx = Fixture::new();
        fx.git("repo");
        fx.write("repo/Cargo.toml", "[workspace]\n");
        let cwd = fx.mkdir("repo/crates/x/src");

        let r = resolver();
        assert_eq!(r.resolve(&cwd).unwrap(), r.resolve(&cwd).unwrap());
    }

    #[test]
    fn a_second_resolve_of_the_same_cwd_is_cached() {
        let fx = Fixture::new();
        fx.git("repo");
        fx.write("repo/package.json", r#"{"name":"app"}"#);
        let cwd = fx.mkdir("repo/src");

        let r = resolver();
        let first = r.resolve(&cwd).expect("first");
        let second = r.resolve(&cwd).expect("cached");
        assert_eq!(first, second);
        assert_eq!(first.root, fx.path("repo"));
    }
}
