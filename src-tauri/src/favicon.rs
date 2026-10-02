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
use std::time::{Duration, SystemTime};

use crate::favicon_parse::{choose_icon, parse_icon_candidates, parse_og_image};

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
/// Also used by `set_app_icon_data` to validate user-uploaded logos.
pub(crate) fn sniff_extension(bytes: &[u8]) -> Option<&'static str> {
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
    enforce_cache_cap(favicons_dir);
    Some(dest)
}

/// Fetch the page's `og:image` as an account thumbnail, cached under
/// `favicons/` with a `thumb-` prefix. Best-effort: any failure (no meta
/// tag, unresolvable URL, download error, non-image bytes) resolves to
/// `None` — a missing thumbnail must never fail account creation.
/// Only raster formats (PNG/JPEG/GIF/WebP): no SVG (nothing rasterizes it
/// here) and no ICO (a multi-size container, poor as a thumbnail).
pub(crate) fn fetch_og_image(page_url: &url::Url, favicons_dir: &Path) -> Option<PathBuf> {
    let (body, _) = http_get(page_url.as_str(), MAX_HTML).ok()?;
    let html = String::from_utf8_lossy(&body);
    let image_url = parse_og_image(&html, page_url)?;
    let (bytes, content_type) = http_get(&image_url, MAX_ICON).ok()?;
    let sniffed = sniff_extension(&bytes);
    let ext = content_type
        .as_deref()
        .and_then(extension_for_content_type)
        .filter(|e| matches!(*e, "png" | "jpg" | "gif" | "webp"))
        .or_else(|| sniffed.filter(|e| matches!(*e, "png" | "jpg" | "gif" | "webp")))?;
    std::fs::create_dir_all(favicons_dir).ok()?;
    let dest = favicons_dir.join(format!("thumb-{}", hash_name(&image_url, ext)));
    if !dest.exists() {
        std::fs::write(&dest, &bytes).ok()?;
    }
    enforce_cache_cap(favicons_dir);
    Some(dest)
}

/// Cap for the `<app-data>/favicons` cache: icons and thumbnails share the
/// directory, so both are counted together.
const MAX_CACHE_FILES: usize = 200;
const MAX_CACHE_BYTES: u64 = 50 * 1024 * 1024;

/// Enforce the favicons cache cap: at most 200 files / 50 MiB total, with
/// the least-recently-modified files evicted first. Called after every
/// successful icon download, og:image fetch, and custom logo save.
/// Best-effort: any I/O failure is swallowed — the cache must never break
/// an icon fetch.
pub(crate) fn enforce_cache_cap(dir: &Path) {
    let entries: Vec<(PathBuf, u64, SystemTime)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                if !meta.is_file() {
                    return None;
                }
                Some((e.path(), meta.len(), meta.modified().ok()?))
            })
            .collect(),
        Err(_) => return,
    };
    let total_bytes: u64 = entries.iter().map(|(_, len, _)| len).sum();
    if entries.len() <= MAX_CACHE_FILES && total_bytes <= MAX_CACHE_BYTES {
        return;
    }
    // Oldest first: evict the least-recently-modified files.
    let mut entries = entries;
    entries.sort_by_key(|(_, _, mtime)| *mtime);
    let mut count = entries.len();
    let mut bytes = total_bytes;
    for (path, len, _) in &entries {
        if count <= MAX_CACHE_FILES && bytes <= MAX_CACHE_BYTES {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            count -= 1;
            bytes = bytes.saturating_sub(*len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    fn tmp_cache(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("appmaka-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cache_stats(dir: &Path) -> (usize, u64) {
        let mut count = 0;
        let mut bytes = 0u64;
        for e in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
            let m = e.metadata().unwrap();
            if m.is_file() {
                count += 1;
                bytes += m.len();
            }
        }
        (count, bytes)
    }

    #[test]
    fn evicts_oldest_files_when_over_count_cap() {
        let dir = tmp_cache("cap-count");
        // 205 small files; the cap is 200.
        for i in 0..205 {
            std::fs::write(dir.join(format!("icon-{i:03}.png")), b"fake").unwrap();
        }
        enforce_cache_cap(&dir);
        let (count, bytes) = cache_stats(&dir);
        assert_eq!(count, 200);
        assert!(bytes <= MAX_CACHE_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn evicts_oldest_files_when_over_size_cap() {
        let dir = tmp_cache("cap-size");
        // Sparse files: logical size counts toward the cap, no real I/O.
        for i in 0..3 {
            let f = File::create(dir.join(format!("big-{i}.png"))).unwrap();
            f.set_len(20 * 1024 * 1024).unwrap();
        }
        enforce_cache_cap(&dir);
        let (count, bytes) = cache_stats(&dir);
        assert!(bytes <= MAX_CACHE_BYTES, "bytes={bytes}");
        assert_eq!(count, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn leaves_cache_alone_when_under_caps() {
        let dir = tmp_cache("cap-under");
        for i in 0..5 {
            std::fs::write(dir.join(format!("icon-{i}.png")), b"fake").unwrap();
        }
        enforce_cache_cap(&dir);
        let (count, _) = cache_stats(&dir);
        assert_eq!(count, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_dir_is_a_noop() {
        enforce_cache_cap(Path::new("/tmp/appmaka-test-does-not-exist-xyz"));
    }
}
