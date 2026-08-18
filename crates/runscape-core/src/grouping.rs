//! Project grouping engine (task T1.3).
//!
//! Turns raw observations into project memberships. Every membership carries
//! evidence and a confidence value; nothing is grouped "because it looked
//! right".
//!
//! Evidence sources, in the priority order from `SPEC.md`:
//!
//! 1. explicit user override
//! 2. Docker Compose project labels
//! 3. Git repository root / workspace root / project manifest (via
//!    [`ProjectResolver`])
//! 4. working-directory ancestry (same resolver)
//! 5. parent/child process relationship — a process with no readable cwd
//!    inherits its parent's project at reduced confidence
//!
//! A process that matches nothing stays ungrouped. That is a legitimate
//! outcome, not a failure to paper over.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::identity::ContainerIdentity;
use crate::ids::ProjectId;
use crate::model::Project;
use crate::project::{NoProject, ProjectEvidence, ProjectResolver, RootKind};

/// One thing to be grouped: a host process or a container.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupingInput {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub cwd: Option<PathBuf>,
    /// Present when the observation is a container.
    pub container: Option<ContainerIdentity>,
}

/// Why a process belongs to a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MembershipEvidence {
    /// The user pinned this path to a project.
    UserOverride { root: PathBuf },
    /// `com.docker.compose.project` label.
    ComposeProject { project: String },
    /// Resolved from the process working directory.
    WorkingDirectory { evidence: Vec<ProjectEvidence> },
    /// Inherited from the parent process because this one disclosed no cwd.
    ParentProcess { parent_pid: u32 },
}

/// A process's membership in a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Membership {
    pub pid: u32,
    pub project_id: ProjectId,
    pub confidence: f32,
    pub evidence: MembershipEvidence,
}

/// Result of grouping one batch of observations.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroupingOutcome {
    /// Projects discovered in this batch, keyed by id.
    pub projects: BTreeMap<ProjectId, Project>,
    pub memberships: Vec<Membership>,
    /// PIDs that could not be attributed to any project.
    pub ungrouped: Vec<u32>,
}

impl GroupingOutcome {
    pub fn membership_for(&self, pid: u32) -> Option<&Membership> {
        self.memberships.iter().find(|m| m.pid == pid)
    }

    pub fn pids_in(&self, project: &ProjectId) -> Vec<u32> {
        self.memberships
            .iter()
            .filter(|m| &m.project_id == project)
            .map(|m| m.pid)
            .collect()
    }
}

/// Confidence applied when a child inherits its parent's project. Lower than
/// any directly resolved membership, because the child may have chdir'd
/// somewhere unrelated before we looked.
const PARENT_INHERITANCE_CONFIDENCE: f32 = 0.60;

/// Confidence for a Compose-labelled container.
const COMPOSE_CONFIDENCE: f32 = 0.90;

/// Confidence for an explicit user override. The user is always right.
const OVERRIDE_CONFIDENCE: f32 = 1.00;

/// Groups observations into projects.
#[derive(Debug, Default)]
pub struct GroupingEngine {
    resolver: ProjectResolver,
    /// Path prefix -> project root, set by the user.
    overrides: Vec<(PathBuf, PathBuf)>,
}

impl GroupingEngine {
    pub fn new(resolver: ProjectResolver) -> Self {
        Self {
            resolver,
            overrides: Vec::new(),
        }
    }

    /// Pin every process whose cwd is under `prefix` to the project at `root`.
    pub fn with_override(mut self, prefix: PathBuf, root: PathBuf) -> Self {
        self.overrides.push((prefix, root));
        self
    }

    /// Group a batch. Order of `inputs` does not affect the outcome: parent
    /// inheritance is resolved in a second pass.
    pub fn group(&self, inputs: &[GroupingInput], at: SystemTime) -> GroupingOutcome {
        let mut outcome = GroupingOutcome::default();
        let mut direct: BTreeMap<u32, ProjectId> = BTreeMap::new();

        for input in inputs {
            if let Some(membership) = self.resolve_direct(input, at, &mut outcome.projects) {
                direct.insert(input.pid, membership.project_id.clone());
                outcome.memberships.push(membership);
            }
        }

        // Second pass: inherit from an already-grouped parent. Only one level of
        // inheritance is applied, so a chain of unreadable processes does not
        // drag a whole subtree into a project on thin evidence.
        for input in inputs {
            if direct.contains_key(&input.pid) {
                continue;
            }
            let inherited = input
                .parent_pid
                .and_then(|parent| direct.get(&parent).map(|id| (parent, id.clone())));

            match inherited {
                Some((parent_pid, project_id)) => outcome.memberships.push(Membership {
                    pid: input.pid,
                    project_id,
                    confidence: PARENT_INHERITANCE_CONFIDENCE,
                    evidence: MembershipEvidence::ParentProcess { parent_pid },
                }),
                None => outcome.ungrouped.push(input.pid),
            }
        }

        outcome.memberships.sort_by_key(|m| m.pid);
        outcome.ungrouped.sort_unstable();
        outcome
    }

    fn resolve_direct(
        &self,
        input: &GroupingInput,
        at: SystemTime,
        projects: &mut BTreeMap<ProjectId, Project>,
    ) -> Option<Membership> {
        // 1. explicit override
        if let Some(cwd) = input.cwd.as_deref()
            && let Some((_, root)) = self
                .overrides
                .iter()
                .find(|(prefix, _)| cwd.starts_with(prefix))
        {
            let project = synthetic_project(root.clone(), RootKind::Package, at);
            let id = project.id.clone();
            projects.entry(id.clone()).or_insert(project);
            return Some(Membership {
                pid: input.pid,
                project_id: id,
                confidence: OVERRIDE_CONFIDENCE,
                evidence: MembershipEvidence::UserOverride { root: root.clone() },
            });
        }

        // 2. Compose project label
        if let Some(compose_project) = input
            .container
            .as_ref()
            .and_then(|c| c.compose_project.clone())
        {
            // A Compose project has no filesystem root we can trust from the
            // container alone, so its identity is the label itself.
            let id = ProjectId::derived(&format!("compose:{compose_project}"));
            projects.entry(id.clone()).or_insert_with(|| Project {
                id: id.clone(),
                root: PathBuf::from(format!("compose://{compose_project}")),
                name: compose_project.clone(),
                kind: RootKind::ComposeStack,
                confidence: COMPOSE_CONFIDENCE,
                evidence: Vec::new(),
                first_seen: at,
                last_seen: at,
            });
            return Some(Membership {
                pid: input.pid,
                project_id: id,
                confidence: COMPOSE_CONFIDENCE,
                evidence: MembershipEvidence::ComposeProject {
                    project: compose_project,
                },
            });
        }

        // 3/4. working-directory resolution
        let cwd = input.cwd.as_deref()?;
        match self.resolver.resolve(cwd) {
            Ok(m) => {
                let confidence = m.confidence;
                let evidence = m.evidence.clone();
                let project = Project::from_match(m, at);
                let id = project.id.clone();
                projects
                    .entry(id.clone())
                    .and_modify(|existing| existing.last_seen = at)
                    .or_insert(project);
                Some(Membership {
                    pid: input.pid,
                    project_id: id,
                    confidence,
                    evidence: MembershipEvidence::WorkingDirectory { evidence },
                })
            }
            Err(
                NoProject::NoMarkers { .. }
                | NoProject::ExcludedRootOnly { .. }
                | NoProject::PathUnavailable { .. },
            ) => None,
        }
    }
}

/// A project the user declared rather than one Runscape discovered.
fn synthetic_project(root: PathBuf, kind: RootKind, at: SystemTime) -> Project {
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    Project {
        id: ProjectId::derived(&root.to_string_lossy()),
        root,
        name,
        kind,
        confidence: OVERRIDE_CONFIDENCE,
        evidence: Vec::new(),
        first_seen: at,
        last_seen: at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ResolverConfig;
    use std::fs;
    use std::time::Duration;

    fn at() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let root = fs::canonicalize(dir.path()).expect("canonicalize");
            Self { _dir: dir, root }
        }

        fn mkdir(&self, rel: &str) -> PathBuf {
            let path = self.root.join(rel);
            fs::create_dir_all(&path).expect("mkdir");
            path
        }

        fn write(&self, rel: &str, contents: &str) {
            let path = self.root.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("mkdir parent");
            }
            fs::write(path, contents).expect("write");
        }
    }

    fn engine() -> GroupingEngine {
        GroupingEngine::new(ProjectResolver::new(ResolverConfig::bare()))
    }

    fn input(pid: u32, parent: Option<u32>, cwd: Option<PathBuf>) -> GroupingInput {
        GroupingInput {
            pid,
            parent_pid: parent,
            cwd,
            container: None,
        }
    }

    #[test]
    fn groups_processes_sharing_a_project_root() {
        let fx = Fixture::new();
        fx.write("repo/package.json", r#"{"name":"repo","workspaces":["*"]}"#);
        let web = fx.mkdir("repo/web");
        let api = fx.mkdir("repo/api");

        let outcome = engine().group(
            &[input(1, None, Some(web)), input(2, None, Some(api))],
            at(),
        );

        assert_eq!(outcome.projects.len(), 1);
        let project = outcome.projects.values().next().expect("project");
        assert_eq!(project.root, fx.root.join("repo"));
        assert_eq!(outcome.pids_in(&project.id), vec![1, 2]);
        assert!(outcome.ungrouped.is_empty());
    }

    #[test]
    fn every_membership_carries_evidence_and_confidence() {
        let fx = Fixture::new();
        fx.write("app/Cargo.toml", "[package]\nname=\"app\"\n");
        let cwd = fx.mkdir("app/src");

        let outcome = engine().group(&[input(7, None, Some(cwd))], at());
        let membership = outcome.membership_for(7).expect("membership");

        assert_eq!(membership.confidence, 0.75);
        match &membership.evidence {
            MembershipEvidence::WorkingDirectory { evidence } => {
                assert!(!evidence.is_empty(), "resolver evidence must be preserved");
            }
            other => panic!("unexpected evidence: {other:?}"),
        }
    }

    #[test]
    fn child_without_cwd_inherits_its_parent_at_lower_confidence() {
        let fx = Fixture::new();
        fx.write("app/package.json", r#"{"name":"app"}"#);
        let cwd = fx.mkdir("app");

        let outcome = engine().group(
            &[input(10, None, Some(cwd)), input(11, Some(10), None)],
            at(),
        );

        let child = outcome.membership_for(11).expect("child membership");
        let parent = outcome.membership_for(10).expect("parent membership");
        assert_eq!(child.project_id, parent.project_id);
        assert!(child.confidence < parent.confidence);
        assert_eq!(
            child.evidence,
            MembershipEvidence::ParentProcess { parent_pid: 10 }
        );
    }

    #[test]
    fn inheritance_does_not_chain_through_ungrouped_processes() {
        let fx = Fixture::new();
        fx.write("app/package.json", r#"{"name":"app"}"#);
        let cwd = fx.mkdir("app");

        // 12 -> 11 -> 10; only 10 has a cwd, so 11 inherits but 12 does not.
        let outcome = engine().group(
            &[
                input(10, None, Some(cwd)),
                input(11, Some(10), None),
                input(12, Some(11), None),
            ],
            at(),
        );

        assert!(outcome.membership_for(11).is_some());
        assert!(outcome.membership_for(12).is_none());
        assert_eq!(outcome.ungrouped, vec![12]);
    }

    #[test]
    fn grouping_is_order_independent() {
        let fx = Fixture::new();
        fx.write("app/package.json", r#"{"name":"app"}"#);
        let cwd = fx.mkdir("app");

        let forward = engine().group(
            &[
                input(10, None, Some(cwd.clone())),
                input(11, Some(10), None),
            ],
            at(),
        );
        let reversed = engine().group(
            &[input(11, Some(10), None), input(10, None, Some(cwd))],
            at(),
        );
        assert_eq!(forward, reversed);
    }

    #[test]
    fn compose_labels_group_containers_without_a_filesystem_root() {
        let container = GroupingInput {
            pid: 0,
            parent_pid: None,
            cwd: None,
            container: Some(ContainerIdentity {
                name: "shop-redis-1".into(),
                compose_project: Some("shop".into()),
                compose_service: Some("redis".into()),
            }),
        };
        let outcome = engine().group(&[container], at());

        let membership = outcome.membership_for(0).expect("membership");
        assert_eq!(membership.confidence, COMPOSE_CONFIDENCE);
        assert_eq!(
            membership.evidence,
            MembershipEvidence::ComposeProject {
                project: "shop".into()
            }
        );
        assert_eq!(
            outcome.projects[&membership.project_id].kind,
            RootKind::ComposeStack
        );
    }

    #[test]
    fn user_override_beats_directory_resolution() {
        let fx = Fixture::new();
        fx.write("app/package.json", r#"{"name":"app"}"#);
        let cwd = fx.mkdir("app/src");
        let pinned = fx.mkdir("elsewhere");

        let outcome = engine()
            .with_override(fx.root.join("app"), pinned.clone())
            .group(&[input(3, None, Some(cwd))], at());

        let membership = outcome.membership_for(3).expect("membership");
        assert_eq!(membership.confidence, OVERRIDE_CONFIDENCE);
        assert_eq!(
            membership.evidence,
            MembershipEvidence::UserOverride {
                root: pinned.clone()
            }
        );
        assert_eq!(outcome.projects[&membership.project_id].root, pinned);
    }

    #[test]
    fn unattributable_processes_stay_ungrouped() {
        let fx = Fixture::new();
        let cwd = fx.mkdir("nowhere/deep");

        let outcome = engine().group(&[input(99, None, Some(cwd))], at());
        assert!(outcome.memberships.is_empty());
        assert_eq!(outcome.ungrouped, vec![99]);
    }

    #[test]
    fn project_evidence_excludes_per_process_cwd_depth() {
        let fx = Fixture::new();
        fx.write("app/package.json", r#"{"name":"app"}"#);
        let cwd = fx.mkdir("app/src/deep");

        let outcome = engine().group(&[input(1, None, Some(cwd))], at());
        let project = outcome.projects.values().next().expect("project");
        assert!(
            !project
                .evidence
                .iter()
                .any(|e| matches!(e, ProjectEvidence::CwdAncestry { .. })),
            "cwd depth is a process fact, not a project fact"
        );
    }
}
