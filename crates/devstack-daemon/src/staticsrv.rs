//! The server of a `kind = "static"` service (plan §3.4): the production
//! bundle of a frontend, built into an artifact, served as files. It runs as
//! the service's process (`devstack serve-static <dir> --port <p>`), so the
//! candidate pipeline, the proxy and the health check treat it like any
//! service, and an end-to-end run pins a build id.

use axum::body::Body;
use axum::http::{HeaderValue, Response, StatusCode, Uri, header};
use std::path::{Component, Path, PathBuf};

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        Some("wasm") => "application/wasm",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

/// The file a request path names below `root`, never outside it.
pub fn resolve(root: &Path, uri_path: &str, spa: bool) -> Option<PathBuf> {
    let decoded = percent_decode(uri_path.split('?').next().unwrap_or(""));
    let mut p = root.to_path_buf();
    for c in Path::new(decoded.trim_start_matches('/')).components() {
        match c {
            Component::Normal(n) => p.push(n),
            Component::CurDir => {}
            _ => return None, // `..`, a drive, a root: not below `root`
        }
    }
    if p.is_dir() {
        p.push("index.html");
    }
    if p.is_file() {
        return Some(p);
    }
    // A client-side route (no file extension): the app's index.html.
    let last = decoded.rsplit('/').next().unwrap_or("");
    (spa && !last.contains('.'))
        .then(|| root.join("index.html"))
        .filter(|i| i.is_file())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) =
                u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Serve `root` on 127.0.0.1:`port` until the process ends.
pub fn serve(root: PathBuf, port: u16, spa: bool) -> Result<(), String> {
    if !root.join("index.html").is_file() && spa {
        return Err(format!("{} has no index.html", root.display()));
    }
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("port {port}: {e}"))?;
        let app = axum::Router::new().fallback(move |uri: Uri| {
            let root = root.clone();
            async move { file(&root, uri.path(), spa).await }
        });
        axum::serve(listener, app).await.map_err(|e| e.to_string())
    })
}

async fn file(root: &Path, path: &str, spa: bool) -> Response<Body> {
    let Some(p) = resolve(root, path, spa) else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from("not found"))
            .unwrap();
    };
    let read = {
        let p = p.clone();
        tokio::task::spawn_blocking(move || std::fs::read(p)).await
    };
    match read.unwrap_or_else(|e| Err(std::io::Error::other(e))) {
        Ok(bytes) => {
            let mut r = Response::new(Body::from(bytes));
            r.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(&p)),
            );
            // index.html names the hashed bundles of this build: never cache it.
            let cache = if p.file_name().is_some_and(|n| n == "index.html") {
                "no-cache"
            } else {
                "public, max-age=3600"
            };
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
            r
        }
        Err(_) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from("cannot read the file"))
            .unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_routes_and_no_escape() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::write(r.join("index.html"), "<app>").unwrap();
        std::fs::create_dir_all(r.join("assets")).unwrap();
        std::fs::write(r.join("assets/main-ab12.js"), "x").unwrap();
        assert_eq!(resolve(r, "/", true), Some(r.join("index.html")));
        assert_eq!(
            resolve(r, "/assets/main-ab12.js", true),
            Some(r.join("assets/main-ab12.js"))
        );
        assert_eq!(
            resolve(r, "/customers/42", true),
            Some(r.join("index.html")),
            "a client route"
        );
        assert_eq!(resolve(r, "/customers/42", false), None, "no SPA fallback");
        assert_eq!(
            resolve(r, "/assets/missing.js", true),
            None,
            "a missing file is a 404"
        );
        assert_eq!(resolve(r, "/../secret.txt", true), None);
        assert_eq!(resolve(r, "/%2e%2e/secret.txt", true), None);
        assert_eq!(content_type(Path::new("a.woff2")), "font/woff2");
    }
}
