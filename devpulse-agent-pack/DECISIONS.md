# DevPulse Architecture Decisions

This is a living ADR-lite document.

## D001 — Rust daemon + T3 frontend

Status: accepted

Rust owns system observation and state.

T3 owns UI.

Reason:

- OS interaction belongs in Rust
- async collectors fit Tokio
- web UI iteration is faster in TypeScript/React
- clean boundary enables future alternate UIs

## D002 — Local-first

Status: accepted

No account or cloud service is required for MVP.

Reason:

- developer environment data is sensitive
- zero-config experience matters
- cloud is unnecessary for the initial product thesis

## D003 — OpenTelemetry is optional

Status: accepted

MVP must work without OTel.

Reason:

The product differentiator is zero-config local discovery.

OTel becomes an enrichment source later.

## D004 — Observation before control

Status: accepted

MVP does not expose arbitrary restart/kill/run commands from the web UI.

Reason:

- reduces security surface
- keeps scope focused
- validates observation model first

## D005 — No packet payload capture

Status: accepted

DevPulse may inspect socket metadata, not packet content.

Reason:

- privacy
- permissions
- complexity
- not required for topology MVP

## D006 — Stable service identity separate from PID

Status: accepted

Reason:

PIDs change after restarts and cannot represent logical services.

## D007 — Evidence on every edge

Status: accepted

Reason:

Automatic topology can be wrong.

The UI must expose how DevPulse knows about a relationship.

## D008 — Deterministic correlations before AI

Status: accepted

The "What changed?" feature begins with temporal/rule-based correlations.

Reason:

- debuggable
- testable
- trustworthy
- avoids premature AI dependency
