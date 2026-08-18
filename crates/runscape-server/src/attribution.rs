//! Ranked resource breakdown. The daemon owns this so the dashboard never
//! invents a hotspot from a thin client-side sort (`AGENTS.md` rule 8).
//!
//! Numbers are what the OS reported this tick. Rankings are not a cause.

use std::cmp::Ordering;

use runscape_core::ids::{ProjectId, ServiceId};
use runscape_core::model::{ProcessInstance, ResourceSample, Service};

use crate::dto::{DominantServiceDto, RankedProcessDto, RankedServiceDto, ResourcePeaksDto};

/// Share of project (or machine-wide grouped) total that counts as "most of it".
const DOMINANT_SHARE: f32 = 0.5;

#[derive(Debug, Clone, Default)]
pub struct RankedLists {
    pub by_cpu: Vec<RankedServiceDto>,
    pub by_memory: Vec<RankedServiceDto>,
    pub dominant_memory: Option<DominantServiceDto>,
}

#[derive(Debug, Clone, Default)]
pub struct ProcessLists {
    pub by_cpu: Vec<RankedProcessDto>,
    pub by_memory: Vec<RankedProcessDto>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TopDto {
    pub limit: usize,
    pub services: ServiceListsDto,
    pub processes: ProcessListsDto,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ServiceListsDto {
    pub by_cpu: Vec<RankedServiceDto>,
    pub by_memory: Vec<RankedServiceDto>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ProcessListsDto {
    pub by_cpu: Vec<RankedProcessDto>,
    pub by_memory: Vec<RankedProcessDto>,
}

pub fn rank_running_services<'a>(
    services: &[&'a Service],
    project_name: impl Fn(&ProjectId) -> Option<&'a str>,
    history_of: impl Fn(&ServiceId) -> Vec<ResourceSample>,
    limit: Option<usize>,
) -> RankedLists {
    let running: Vec<&Service> = services
        .iter()
        .copied()
        .filter(|s| s.is_running())
        .collect();
    let measured: Vec<&Service> = running
        .iter()
        .copied()
        .filter(|s| s.resources_measured())
        .collect();
    let cpu_total: f32 = measured.iter().map(|s| s.total_cpu_percent()).sum();
    let memory_total: u64 = measured.iter().map(|s| s.total_memory_bytes()).sum();

    let mut by_cpu: Vec<RankedServiceDto> = running
        .iter()
        .map(|s| {
            let name = s.project_id.as_ref().and_then(&project_name);
            let history = history_of(&s.id);
            ranked_service(s, share(s.total_cpu_percent(), cpu_total), name, &history)
        })
        .collect();
    by_cpu.sort_by(|a, b| cmp_desc_f32(a.cpu_percent, b.cpu_percent).then(a.name.cmp(&b.name)));

    let mut by_memory: Vec<RankedServiceDto> = running
        .iter()
        .map(|s| {
            let name = s.project_id.as_ref().and_then(&project_name);
            let history = history_of(&s.id);
            ranked_service(
                s,
                share_u64(s.total_memory_bytes(), memory_total),
                name,
                &history,
            )
        })
        .collect();
    by_memory.sort_by(|a, b| {
        b.memory_bytes
            .cmp(&a.memory_bytes)
            .then(a.name.cmp(&b.name))
    });

    if let Some(limit) = limit {
        by_cpu.truncate(limit);
        by_memory.truncate(limit);
    }

    let dominant_memory = by_memory
        .first()
        .filter(|top| measured.len() >= 2 && top.share >= DOMINANT_SHARE && top.resources_measured)
        .map(|top| DominantServiceDto {
            id: top.id.clone(),
            name: top.name.clone(),
            share: top.share,
        });

    RankedLists {
        by_cpu,
        by_memory,
        dominant_memory,
    }
}

pub fn rank_running_processes<'a>(
    services: &[&'a Service],
    project_name: impl Fn(&ProjectId) -> Option<&'a str>,
    limit: usize,
) -> ProcessLists {
    let mut rows: Vec<RankedProcessDto> = Vec::new();
    for service in services.iter().copied().filter(|s| s.is_running()) {
        let project_id = service.project_id.as_ref().map(ToString::to_string);
        let pname = service
            .project_id
            .as_ref()
            .and_then(&project_name)
            .map(str::to_owned);
        for instance in &service.instances {
            rows.push(ranked_process(
                instance,
                service,
                project_id.clone(),
                pname.clone(),
            ));
        }
    }

    let mut by_cpu = rows.clone();
    by_cpu.sort_by(|a, b| cmp_desc_f32(a.cpu_percent, b.cpu_percent).then(a.pid.cmp(&b.pid)));
    by_cpu.truncate(limit);

    let mut by_memory = rows;
    by_memory.sort_by(|a, b| b.memory_bytes.cmp(&a.memory_bytes).then(a.pid.cmp(&b.pid)));
    by_memory.truncate(limit);

    ProcessLists { by_cpu, by_memory }
}

fn ranked_service(
    service: &Service,
    share: f32,
    project_name: Option<&str>,
    history: &[ResourceSample],
) -> RankedServiceDto {
    let peaks = ResourceSample::peaks(history).map(ResourcePeaksDto::from);
    RankedServiceDto {
        id: service.id.to_string(),
        name: service.name.clone(),
        project_id: service.project_id.as_ref().map(ToString::to_string),
        project_name: project_name.map(str::to_owned),
        cpu_percent: service.total_cpu_percent(),
        memory_bytes: service.total_memory_bytes(),
        share,
        process_count: service.instances.len(),
        resources_measured: service.resources_measured(),
        peak_cpu_percent: peaks.as_ref().map(|p| p.cpu_percent),
        peak_cpu_at: peaks.as_ref().map(|p| p.cpu_at.clone()),
        peak_memory_bytes: peaks.as_ref().map(|p| p.memory_bytes),
        peak_memory_at: peaks.as_ref().map(|p| p.memory_at.clone()),
    }
}

fn ranked_process(
    instance: &ProcessInstance,
    service: &Service,
    project_id: Option<String>,
    project_name: Option<String>,
) -> RankedProcessDto {
    RankedProcessDto {
        pid: instance.pid,
        parent_pid: instance.parent_pid,
        name: instance_label(instance),
        command: instance.command.clone(),
        cwd: instance.cwd.clone(),
        service_id: service.id.to_string(),
        service_name: service.name.clone(),
        project_id,
        project_name,
        cpu_percent: instance.cpu_percent,
        memory_bytes: instance.memory_bytes,
        virtual_memory_bytes: instance.virtual_memory_bytes,
        thread_count: instance.thread_count,
        disk_read_bytes: instance.disk_read_bytes,
        disk_write_bytes: instance.disk_write_bytes,
    }
}

fn instance_label(instance: &ProcessInstance) -> String {
    instance
        .executable
        .as_ref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .or_else(|| instance.command.first().cloned())
        .unwrap_or_else(|| format!("pid {}", instance.pid))
}

fn share(part: f32, total: f32) -> f32 {
    if total <= 0.0 { 0.0 } else { part / total }
}

fn share_u64(part: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        part as f32 / total as f32
    }
}

fn cmp_desc_f32(a: f32, b: f32) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    use runscape_core::identity::Runtime;
    use runscape_core::ids::ServiceId;
    use runscape_core::model::{Health, ProcessInstance, ServiceKind};

    fn instance(pid: u32, cpu: f32, mem: u64) -> ProcessInstance {
        ProcessInstance {
            pid,
            parent_pid: None,
            executable: Some(PathBuf::from("/usr/bin/node")),
            command: vec!["node".into()],
            cwd: None,
            started_at_epoch_secs: 0,
            cpu_percent: cpu,
            memory_bytes: mem,
            virtual_memory_bytes: mem * 2,
            thread_count: Some(4),
            disk_read_bytes: 10,
            disk_write_bytes: 2,
        }
    }

    fn host(name: &str, cpu: f32, mem: u64) -> Service {
        Service {
            id: ServiceId::derived(name),
            project_id: Some(ProjectId::derived("/tmp/app")),
            name: name.into(),
            kind: ServiceKind::HostProcess,
            runtime: Runtime::Node,
            fingerprint: format!("host|{name}"),
            health: Health::Healthy,
            instances: vec![instance(10, cpu, mem)],
            endpoints: vec![],
            first_seen: UNIX_EPOCH,
            last_seen: UNIX_EPOCH,
            restart_count: 0,
            measured_cpu_percent: None,
            measured_memory_bytes: None,
        }
    }

    fn container(name: &str, cpu: Option<f32>, mem: Option<u64>) -> Service {
        Service {
            id: ServiceId::derived(name),
            project_id: Some(ProjectId::derived("/tmp/app")),
            name: name.into(),
            kind: ServiceKind::Container(runscape_core::ContainerIdentity {
                name: name.into(),
                compose_project: None,
                compose_service: None,
            }),
            runtime: Runtime::Container,
            fingerprint: format!("container|{name}"),
            health: Health::Healthy,
            instances: vec![],
            endpoints: vec![],
            first_seen: UNIX_EPOCH,
            last_seen: UNIX_EPOCH,
            restart_count: 0,
            measured_cpu_percent: cpu,
            measured_memory_bytes: mem,
        }
    }

    #[test]
    fn memory_rank_gives_share_of_measured_total() {
        let web = host("web", 10.0, 80);
        let api = host("api", 5.0, 20);
        let ranked = rank_running_services(&[&web, &api], |_| Some("app"), |_| Vec::new(), None);
        assert_eq!(ranked.by_memory[0].name, "web");
        assert!((ranked.by_memory[0].share - 0.8).abs() < 0.001);
        assert_eq!(ranked.by_cpu[0].name, "web");
        assert!((ranked.by_cpu[0].share - 10.0 / 15.0).abs() < 0.001);
        assert_eq!(
            ranked.dominant_memory.as_ref().map(|d| d.name.as_str()),
            Some("web")
        );
    }

    #[test]
    fn unmeasured_container_is_listed_but_not_a_zero_idle() {
        let web = host("web", 1.0, 10);
        let db = container("db", None, None);
        let ranked = rank_running_services(&[&web, &db], |_| Some("app"), |_| Vec::new(), None);
        let db_row = ranked
            .by_memory
            .iter()
            .find(|s| s.name == "db")
            .expect("db listed");
        assert!(!db_row.resources_measured);
        assert_eq!(db_row.memory_bytes, 0);
        assert_eq!(db_row.share, 0.0);
        assert_eq!(ranked.by_memory[0].name, "web");
    }

    #[test]
    fn process_rank_uses_instances_not_the_whole_os_table() {
        let web = host("web", 9.0, 50);
        let lists = rank_running_processes(&[&web], |_| Some("app"), 10);
        assert_eq!(lists.by_cpu.len(), 1);
        assert_eq!(lists.by_cpu[0].pid, 10);
        assert_eq!(lists.by_cpu[0].service_name, "web");
        assert_eq!(lists.by_cpu[0].name, "node");
    }

    #[test]
    fn limit_truncates_after_sort() {
        let a = host("a", 1.0, 1);
        let b = host("b", 3.0, 3);
        let c = host("c", 2.0, 2);
        let ranked = rank_running_services(&[&a, &b, &c], |_| Some("app"), |_| Vec::new(), Some(2));
        assert_eq!(ranked.by_cpu.len(), 2);
        assert_eq!(ranked.by_cpu[0].name, "b");
        assert_eq!(ranked.by_cpu[1].name, "c");
    }
}
