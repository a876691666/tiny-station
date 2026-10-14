//! The embedded management UI.
//!
//! The Vue single-page app is built from `web/` into `web/dist` and embedded
//! into the binary at compile time (`build.rs` guarantees the folder exists).
//! Assets are served here; unknown paths fall back to `index.html` so client
//! routes (history mode) work.

use axum::body::Body;
use axum::http::{header, StatusCode};
use axum::response::Response;
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "web/dist"]
struct Assets;

/// Serve an embedded file, falling back to the SPA shell for unknown paths.
pub fn serve(path: &str) -> Response {
    let path = path.trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => asset(path, file.data.into_owned()),
        None => match Assets::get("index.html") {
            Some(shell) => html(shell.data.into_owned()),
            None => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(Body::from("UI 未构建:请在 web/ 下执行 `pnpm build`"))
                .unwrap(),
        },
    }
}

fn asset(path: &str, data: Vec<u8>) -> Response {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    // Vite hashes asset filenames, so they can be cached forever; index.html
    // must not be.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CACHE_CONTROL, cache)
        .body(Body::from(data))
        .unwrap()
}

fn html(data: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(data))
        .unwrap()
}
