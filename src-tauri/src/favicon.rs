//! Web-app favicon fetching with local caching.
//!
//! When an app is added, the frontend asks for its logo via the
//! `fetch_favicon` command. This fetches the site's HTML, parses
//! `<link rel="icon" / "apple-touch-icon">` (see `favicon_parse.rs`,
//! resolving relative URLs and preferring the largest declared size), falls
//! back to `/favicon.ico`, downloads the image, and caches it under
//! `<app-data>/favicons/`. The cached local path is stored on the app
//! record, so tiles keep their logo offline and never depend on the site
//! allowing hot-linking. Everything is best-effort: any failure resolves to
//! `None` and the UI keeps its fallbacks.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::favicon_parse::{choose_icon, parse_icon_candidates};

const MAX_HTML: u64 = 512 * 1024;
const MAX_ICON: u64 = 2 * 1024 * 1024;
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

fn http_get(url: &str, max_bytes: u64) -> Result<(Vec<u8>, Option<String>), String> {
    let resp = ureq::get(url)
        .set("User-Agent", UA)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("request failed: {e}"))?;
    let content_type = resp.header("Content-Type").map(str::to_string);
    let mut body = Vec::new();
    resp.into_reader()
        .take(max_bytes)
        .read_to_end(&mut body)
        .map_err(|e| format!("read failed: {e}"))?;
    if body.is_empty() {
        return Err("empty response".to_string());
    }
    Ok((body, content_type))
}

fn extension_for_content_type(ct: &str) -> Option<&'static str> {
    let ct = ct.split(';').next().unwrap_or("").trim().to_lowercase();
    match ct.as_str() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/svg+xml" => Some("svg"),
        "image/x-icon" | "image/vnd.microsoft.icon" => Some("ico"),
        _ => None,
    }
}

/// Magic-byte sniffing, for servers that send the wrong (or no)
/// Content-Type. An HTML error page never sniffs as an image.
fn sniff_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(b"GIF8") {
        Some("gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        Some("ico")
    } else {
        None
    }
}

fn hash_name(url: &str, ext: &str) -> String {
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}.{ext}", h.finish())
}

/// Download the best icon for `page_url` and cache it under `favicons_dir`.
/// Returns the absolute path of the cached file, or `None` when nothing
/// usable was found. Pure blocking I/O — the command runs it off the IPC
/// thread.
pub(crate) fn download_icon(page_url: &url::Url, favicons_dir: &Path) -> Option<PathBuf> {
    // 1. Page HTML → icon candidates.
    let icon_url = match http_get(page_url.as_str(), MAX_HTML) {
        Ok((body, _)) => {
            let html = String::from_utf8_lossy(&body);
            let candidates = parse_icon_candidates(&html, page_url);
            choose_icon(&candidates).map(|c| c.href.clone())
        }
        Err(_) => None,
    };
    // 2. Fall back to /favicon.ico.
    let icon_url = icon_url.or_else(|| page_url.join("/favicon.ico").ok().map(|u| u.to_string()))?;
    // 3. Download; require image bytes (content type, magic bytes, or a
    // well-known image extension in the URL — in that order).
    let (bytes, content_type) = http_get(&icon_url, MAX_ICON).ok()?;
    let ext = content_type
        .as_deref()
        .and_then(extension_for_content_type)
        .or_else(|| sniff_extension(&bytes))
        .or_else(|| {
            let path = icon_url.rsplit('/').next().unwrap_or("");
            match path.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
                "png" => Some("png"),
                "jpg" | "jpeg" => Some("jpg"),
                "gif" => Some("gif"),
                "webp" => Some("webp"),
                "svg" => Some("svg"),
                "ico" => Some("ico"),
                _ => None,
            }
        })?;
    std::fs::create_dir_all(favicons_dir).ok()?;
    let dest = favicons_dir.join(hash_name(&icon_url, ext));
    // Same icon URL → same filename, so a re-fetch refreshes nothing and a
    // changed icon URL naturally gets its own cache entry.
    if !dest.exists() {
        std::fs::write(&dest, &bytes).ok()?;
    }
    Some(dest)
}
