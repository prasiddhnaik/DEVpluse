/**
 * Wire types, mirroring `docs/api-contract.md` and `crates/devpulse-server/src/dto.rs`.
 *
 * These are the *daemon's* shapes, deliberately not remodelled: the daemon owns
 * runtime truth and the dashboard renders it (`AGENTS.md` rule 8). If a field is
 * missing here, the answer is to add it to the contract, not to compute it in
 * TypeScript.
 */

export type Health = "healthy" | "degraded" | "stopped" | "unknown";

export type EvidenceType =
  | "observed_socket"
  | "docker_network"
  | "configured"
  | "otel_span"
  | "inferred";

export interface Evidence {
  evidence_type: EvidenceType;
  /** 0.0 to 1.0. */
  confidence: number;
  first_seen: string;
  last_seen: string;
  detail: string | null;
}

export interface Connection {
  id: string;
  source: string;
  target: string;
  target_port: number;
  evidence: Evidence;
}

export interface Endpoint {
  address: string;
  port: number;
  protocol: "tcp" | "udp";
  pid: number | null;
}

export interface ProcessInstance {
  pid: number;
  parent_pid: number | null;
  executable: string | null;
  /** Already redacted by the daemon; the raw argv never leaves the process. */
  command: string[];
  cwd: string | null;
  started_at: string;
  cpu_percent: number;
  memory_bytes: number;
}

export interface ResourceSample {
  at: string;
  cpu_percent: number;
  memory_bytes: number;
}

export type ServiceKind =
  | { kind: "host_process" }
  | {
      kind: "container";
      name: string;
      compose_project: string | null;
      compose_service: string | null;
    };

export interface Service {
  id: string;
  project_id: string | null;
  name: string;
  kind: ServiceKind;
  runtime: string;
  fingerprint: string;
  health: Health;
  restart_count: number;
  first_seen: string;
  last_seen: string;
  instances: ProcessInstance[];
  endpoints: Endpoint[];
  resource_history?: ResourceSample[];
}

export interface ServiceDetail extends Service {
  connections: { outbound: Connection[]; inbound: Connection[] };
  recent_events: DevPulseEvent[];
}

export type Severity = "info" | "warning" | "critical";

export interface Warning {
  id: string;
  rule: string;
  severity: Severity;
  project_id: string | null;
  service_id: string | null;
  message: string;
  first_seen: string;
  last_seen: string;
  related_events: string[];
}

export interface ProjectSummary {
  id: string;
  name: string;
  root: string;
  kind: string;
  confidence: number;
  first_seen: string;
  last_seen: string;
  service_count: number;
  running_service_count: number;
  health: Health;
  memory_bytes: number;
  cpu_percent: number;
  recent_warning: Warning | null;
}

export interface ProjectDetail {
  project: ProjectSummary;
  services: Service[];
  connections: Connection[];
  warnings: Warning[];
  recent_events: DevPulseEvent[];
}

export type EventKind =
  | { type: "project_detected"; project_id: string }
  | { type: "service_started"; service_id: string; pid: number | null }
  | { type: "service_stopped"; service_id: string; pid: number | null }
  | {
      type: "service_restarted";
      service_id: string;
      old_pid: number | null;
      new_pid: number | null;
    }
  | { type: "port_opened"; service_id: string | null; port: number }
  | { type: "port_closed"; service_id: string | null; port: number }
  | {
      type: "connection_started";
      connection_id: string;
      source: string;
      target: string;
      target_port: number;
    }
  | { type: "connection_ended"; connection_id: string }
  | { type: "health_changed"; service_id: string; from: Health; to: Health }
  | { type: "resource_warning"; service_id: string; detail: string }
  | { type: "file_changed"; project_id: string; path: string };

export interface DevPulseEvent {
  id: string;
  at: string;
  project_id: string | null;
  kind: EventKind;
}

/** Why an event appears in another's context. Never "caused by". */
export type Relation =
  | "same_service"
  | "same_project"
  | "preceding_file_change"
  | "temporal";

export interface RelatedEvent extends DevPulseEvent {
  relation: Relation;
  /** Negative is before the anchor. */
  offset_ms: number;
}

export interface EventContext {
  event: DevPulseEvent;
  window_ms: number;
  before: RelatedEvent[];
  after: RelatedEvent[];
}

export interface GraphNode {
  id: string;
  name: string;
  runtime: string;
  health: Health;
  port: number | null;
  cpu_percent: number;
  memory_bytes: number;
  kind: "host_process" | "container";
}

export interface Graph {
  project_id: string;
  nodes: GraphNode[];
  edges: Connection[];
}

export type Support = "full" | "same_user_only" | "unavailable";

export interface CollectorStatus {
  last_duration_ms: number;
  last_run: string | null;
  degraded_fields?: Record<string, number>;
  sockets_without_owner?: number;
  error?: string;
}

export interface Status {
  version: string;
  started_at: string;
  uptime_ms: number;
  platform: {
    os: string;
    process_list: Support;
    process_cwd: Support;
    process_exe: Support;
    process_command: Support;
    socket_list: Support;
    socket_owner_pid: Support;
    root_widens_view: boolean;
    notes: string[];
  };
  docker: { available: boolean; reason?: string };
  counts: {
    projects: number;
    services: number;
    connections: number;
    events: number;
  };
  collectors: {
    process: CollectorStatus;
    socket: CollectorStatus;
    container?: CollectorStatus;
  };
}

/** Frames the daemon pushes over `/ws/v1`. */
export type ServerFrame =
  | {
      type: "snapshot";
      at: string;
      status: Status;
      projects: ProjectSummary[];
      services: Service[];
      connections: Connection[];
      warnings: Warning[];
    }
  | { type: "events"; at: string; events: DevPulseEvent[] }
  | {
      type: "services_changed";
      at: string;
      services: Service[];
      removed: string[];
    }
  | {
      type: "topology_changed";
      at: string;
      project_id: string | null;
      added: Connection[];
      removed: string[];
    }
  | {
      type: "warnings_changed";
      at: string;
      warnings: Warning[];
      removed: string[];
    };
