//! HTTP API (tasks T3.1–T3.3).
//!
//! Read-only by construction: every route is a `GET`, no route accepts a path,
//! a command, or anything else that could turn the daemon into a file reader or
//! an executor (`DECISIONS.md` D004, `docs/api-contract.md`).

use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, middleware};
use devpulse_core::ids::{ProjectId, ServiceId};
use devpulse_events::correlation::{self, CONTEXT_WINDOW};
use serde::Deserialize;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use crate::dto::{
    ApiErrorDto, ConnectionDto, EventContextDto, EventDto, GraphDto, GraphNodeDto,
    ProjectDetailDto, ProjectSummaryDto, RelatedEventDto, ServiceConnectionsDto, ServiceDetailDto,
    StatusDto, WarningDto, parse_rfc3339,
};
use crate::security::OriginPolicy;
use crate::state::{AppState, DEFAULT_EVENT_LIMIT, EventFilter, MAX_EVENT_LIMIT, RuntimeView};
use crate::ws;

/// Longest context window a caller may ask for. Beyond this, "around this
/// event" stops meaning anything.
const MAX_CONTEXT_WINDOW: Duration = Duration::from_secs(600);

/// Events attached to a project detail response.
const PROJECT_EVENT_LIMIT: usize = 100;
/// Events attached to a service detail response.
const SERVICE_EVENT_LIMIT: usize = 50;

/// Build the daemon's router.
pub fn router(state: AppState, policy: OriginPolicy) -> Router {
    let origins: Vec<header::HeaderValue> = policy
        .allowed_origins()
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([axum::http::Method::GET]);

    Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/projects", get(projects))
        .route("/api/v1/projects/{id}", get(project))
        .route("/api/v1/services/{id}", get(service))
        .route("/api/v1/graph/{project_id}", get(graph))
        .route("/api/v1/events", get(events))
        .route("/api/v1/events/{id}/context", get(event_context))
        .route("/api/v1/warnings", get(warnings))
        .route("/ws/v1", get(ws::upgrade))
        .layer(middleware::from_fn_with_state(policy, enforce_origin))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

/// Reject browser requests from origins that are not on the allow-list.
///
/// CORS alone would stop the *page* from reading the response, but the daemon
/// would still have done the work and the answer would still have left the
/// process. A daemon that can enumerate a developer's processes should not
/// answer at all (`AGENTS.md` rule 6).
async fn enforce_origin(
    State(policy): State<OriginPolicy>,
    request: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());

    if policy.permits(origin) {
        next.run(request).await
    } else {
        ApiError::forbidden("origin not allowed").into_response()
    }
}

async fn status(State(state): State<AppState>) -> Json<StatusDto> {
    Json(state.status().await)
}

async fn projects(State(state): State<AppState>) -> Json<Vec<ProjectSummaryDto>> {
    let view = state.view().await;
    Json(view.project_summaries())
}

async fn project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ProjectDetailDto>, ApiError> {
    let view = state.view().await;
    let project_id = ProjectId::from_stored(id.clone());
    let project = view
        .projects
        .get(&project_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown project {id}")))?;

    let services = view.services_of(&project_id);
    Ok(Json(ProjectDetailDto {
        project: view.project_summary(project),
        services: services.map(|s| view.service_dto(s)).collect(),
        connections: view
            .connections_of(&project_id)
            .into_iter()
            .map(ConnectionDto::from)
            .collect(),
        warnings: view
            .warnings_of(&project_id)
            .into_iter()
            .map(WarningDto::from)
            .collect(),
        recent_events: recent_events(
            &view,
            EventFilter::for_project(&project_id, PROJECT_EVENT_LIMIT),
        ),
    }))
}

async fn service(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ServiceDetailDto>, ApiError> {
    let view = state.view().await;
    let service_id = ServiceId::from_stored(id.clone());
    let service = view
        .services
        .get(&service_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown service {id}")))?;

    let (outbound, inbound) = view.connections_touching(&service_id);
    Ok(Json(ServiceDetailDto {
        service: view.service_dto(service),
        connections: ServiceConnectionsDto {
            outbound: outbound.into_iter().map(ConnectionDto::from).collect(),
            inbound: inbound.into_iter().map(ConnectionDto::from).collect(),
        },
        recent_events: recent_events(
            &view,
            EventFilter::for_service(&service_id, SERVICE_EVENT_LIMIT),
        ),
    }))
}

async fn graph(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
) -> Result<Json<GraphDto>, ApiError> {
    let view = state.view().await;
    let id = ProjectId::from_stored(project_id.clone());
    if !view.projects.contains_key(&id) {
        return Err(ApiError::not_found(format!("unknown project {project_id}")));
    }

    Ok(Json(GraphDto {
        project_id: id.to_string(),
        nodes: view.services_of(&id).map(GraphNodeDto::from).collect(),
        edges: view
            .connections_of(&id)
            .into_iter()
            .map(ConnectionDto::from)
            .collect(),
    }))
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    project_id: Option<String>,
    service_id: Option<String>,
    limit: Option<usize>,
    since: Option<String>,
}

async fn events(
    State(state): State<AppState>,
    Query(query): Query<EventsQuery>,
) -> Result<Json<Vec<EventDto>>, ApiError> {
    let since = match query.since.as_deref() {
        None => None,
        Some(raw) => Some(
            parse_rfc3339(raw)
                .ok_or_else(|| ApiError::bad_request(format!("since is not RFC 3339: {raw}")))?,
        ),
    };

    let filter = EventFilter {
        project_id: query.project_id.map(ProjectId::from_stored),
        service_id: query.service_id.map(ServiceId::from_stored),
        since,
        // An out-of-range limit is clamped, not rejected: the caller still gets
        // useful data and the daemon still bounds the work.
        limit: query
            .limit
            .unwrap_or(DEFAULT_EVENT_LIMIT)
            .clamp(1, MAX_EVENT_LIMIT),
    };

    let view = state.view().await;
    Ok(Json(recent_events(&view, filter)))
}

#[derive(Debug, Deserialize)]
struct ContextQuery {
    window_ms: Option<u64>,
}

/// `GET /api/v1/events/:id/context` — what happened around one event (T7.4).
async fn event_context(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<ContextQuery>,
) -> Result<Json<EventContextDto>, ApiError> {
    let window = Duration::from_millis(
        query
            .window_ms
            .unwrap_or(CONTEXT_WINDOW.as_millis() as u64)
            .clamp(1_000, MAX_CONTEXT_WINDOW.as_millis() as u64),
    );

    let view = state.view().await;
    let anchor = view
        .events
        .iter()
        .find(|event| event.id.as_str() == id)
        .ok_or_else(|| ApiError::not_found(format!("unknown event {id}")))?;

    // The ring is the corpus: the context of an event is what the daemon saw
    // around it, and it saw it in this window.
    let all: Vec<_> = view.events.iter().cloned().collect();
    let context = correlation::context(&all, anchor, window);

    Ok(Json(EventContextDto {
        event: EventDto::from(&context.anchor),
        window_ms: window.as_millis() as u64,
        before: context.before.iter().map(related_dto).collect(),
        after: context.after.iter().map(related_dto).collect(),
    }))
}

fn related_dto(related: &correlation::RelatedEvent) -> RelatedEventDto {
    RelatedEventDto {
        event: EventDto::from(&related.event),
        relation: match related.relation {
            correlation::Relation::SameService => "same_service",
            correlation::Relation::SameProject => "same_project",
            correlation::Relation::PrecedingFileChange => "preceding_file_change",
            correlation::Relation::Temporal => "temporal",
        }
        .to_string(),
        offset_ms: related.offset_ms,
    }
}

#[derive(Debug, Deserialize)]
struct WarningsQuery {
    project_id: Option<String>,
}

/// `GET /api/v1/warnings` — every active warning, newest activity first.
async fn warnings(
    State(state): State<AppState>,
    Query(query): Query<WarningsQuery>,
) -> Json<Vec<WarningDto>> {
    let view = state.view().await;
    let warnings = match query.project_id.map(ProjectId::from_stored) {
        Some(project) => view.warnings_of(&project),
        None => view.warnings.iter().collect(),
    };
    Json(warnings.into_iter().map(WarningDto::from).collect())
}

fn recent_events(view: &RuntimeView, filter: EventFilter) -> Vec<EventDto> {
    view.recent_events(&filter)
        .into_iter()
        .map(EventDto::from)
        .collect()
}

/// API errors carry the closed `code` set from the contract.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: ApiErrorDto,
}

impl ApiError {
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            body: ApiErrorDto::new("not_found", message),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: ApiErrorDto::new("bad_request", message),
        }
    }

    /// `bad_request` is the contract's code for a request the daemon refuses;
    /// the HTTP status carries the "forbidden" part.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            body: ApiErrorDto::new("bad_request", message),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        (self.status, headers, Json(self.body)).into_response()
    }
}
