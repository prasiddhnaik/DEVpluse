//! Static dashboard files baked into the daemon binary.
//!
//! `runscape serve` (not `--headless` advertising) serves these at `/` so a
//! `cargo install` user can open the visual UI without running Next. Rebuild
//! them with `cd apps/web && bun run export:daemon`.

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "dashboard/"]
struct Assets;

/// Serve an embedded file, or the HTML shell for client-side routes.
pub async fn file(uri: Uri) -> Response {
    let Some(path) = safe_path(uri.path()) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    if let Some(response) = asset(&path) {
        return response;
    }
    if looks_like_asset(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    asset("index.html").unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

fn asset(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = mime_of(path);
    Some(
        (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, cache_control(path)),
            ],
            file.data.into_owned(),
        )
            .into_response(),
    )
}

fn safe_path(uri_path: &str) -> Option<String> {
    let stripped = uri_path.trim_start_matches('/');
    if stripped.split('/').any(|segment| segment == "..") {
        return None;
    }
    if stripped.is_empty() {
        Some("index.html".into())
    } else {
        Some(stripped.to_string())
    }
}

fn looks_like_asset(path: &str) -> bool {
    path.starts_with("_next/") || path.contains('.')
}

fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("json") | Some("map") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn cache_control(path: &str) -> &'static str {
    if path.starts_with("_next/static/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_index() {
        assert_eq!(safe_path("/").as_deref(), Some("index.html"));
    }

    #[test]
    fn parent_segments_are_rejected() {
        assert_eq!(safe_path("/foo/../secret"), None);
    }

    #[test]
    fn hashed_chunks_are_assets() {
        assert!(looks_like_asset("_next/static/chunks/app.js"));
        assert!(!looks_like_asset("projects/prj_abc"));
    }
}
