//! The server of a `kind = "static"` service (plan §3.4): the production
//! bundle of a frontend, built into an artifact, served as files. It runs as
//! the service's process (`devstack serve-static <dir> --port <p>`), so the
//! candidate pipeline, the proxy and the health check treat it like any
//! service, and an end-to-end run pins a build id.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode, Uri, header};
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
        let app = axum::Router::new().fallback(move |uri: Uri, headers: HeaderMap| {
            let root = root.clone();
            async move {
                let tag = headers
                    .get(header::IF_NONE_MATCH)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                file(&root, uri.path(), spa, tag.as_deref()).await
            }
        });
        axum::serve(listener, app).await.map_err(|e| e.to_string())
    })
}

/// Whether a file's name carries a hash of its content (`main-3f2a1b9c.js`,
/// `index.Bd3x9_kQ.css`), so a changed file has a new name: the last part of
/// its stem, after `-` or `.`, 8 to 32 letters, digits or `_`, with a digit.
fn content_hashed(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let Some((_, last)) = stem.rsplit_once(['-', '.']) else {
        return false;
    };
    (8..=32).contains(&last.len())
        && last.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && last.chars().any(|c| c.is_ascii_digit())
}

/// How a file may be cached: one whose name has its content hash for a year;
/// any other (`index.html`, `config.json`, a manifest) asked again each time,
/// answered `304` while its `ETag` holds, so a new build shows at once.
fn cache_control(path: &Path) -> &'static str {
    if content_hashed(path) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Whether `If-None-Match` names `etag` (or `*`).
fn matches_etag(if_none_match: &str, etag: &str) -> bool {
    if_none_match
        .split(',')
        .map(|t| t.trim().trim_start_matches("W/"))
        .any(|t| t == etag || t == "*")
}

async fn file(root: &Path, path: &str, spa: bool, if_none_match: Option<&str>) -> Response<Body> {
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
            let etag = format!("\"{}\"", &blake3::hash(&bytes).to_hex()[..32]);
            let fresh = if_none_match.is_some_and(|t| matches_etag(t, &etag));
            let mut r = if fresh {
                let mut r = Response::new(Body::empty());
                *r.status_mut() = StatusCode::NOT_MODIFIED;
                r
            } else {
                Response::new(Body::from(bytes))
            };
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(&p)),
            );
            h.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(cache_control(&p)),
            );
            if let Ok(v) = HeaderValue::from_str(&etag) {
                h.insert(header::ETAG, v);
            }
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

    #[test]
    fn only_content_hashed_names_are_cached_for_long() {
        for hashed in [
            "assets/index-Bd3x9_kQ.js",
            "main.3f2a1b9c.js",
            "chunk-1a2b3c4d5e6f.css",
        ] {
            assert_eq!(
                cache_control(Path::new(hashed)),
                "public, max-age=31536000, immutable",
                "{hashed}"
            );
        }
        for plain in [
            "index.html",
            "assets/config.json",
            "manifest.webmanifest",
            "i18n/de.json",
            "polyfills.js",
            "chunk-vendors.js",
            "favicon.ico",
        ] {
            assert_eq!(cache_control(Path::new(plain)), "no-cache", "{plain}");
        }
        assert!(matches_etag("\"a\", W/\"b\"", "\"b\""));
        assert!(matches_etag("*", "\"b\""));
        assert!(!matches_etag("\"a\"", "\"b\""));
    }

    #[tokio::test]
    async fn an_unchanged_file_is_not_sent_again() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.json"), "{}").unwrap();
        let r = file(d.path(), "/config.json", false, None).await;
        assert_eq!(r.status(), StatusCode::OK);
        let etag = r.headers()[header::ETAG].to_str().unwrap().to_string();
        let r = file(d.path(), "/config.json", false, Some(&etag)).await;
        assert_eq!(r.status(), StatusCode::NOT_MODIFIED);
        std::fs::write(d.path().join("config.json"), "{\"a\":1}").unwrap();
        let r = file(d.path(), "/config.json", false, Some(&etag)).await;
        assert_eq!(r.status(), StatusCode::OK, "changed: sent again");
    }
}
