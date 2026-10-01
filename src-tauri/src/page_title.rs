//! Page-title fetch for the quick-add flow.
//!
//! The user pastes a URL; this does a plain HTTP GET (browser user-agent,
//! 10 s timeout, body capped at 512 KiB since the title is always near the
//! top) and extracts `<title>`. Every failure is a plain-string error the
//! frontend can show; the UI falls back to a prettified domain name, so this
//! is best-effort by design. It never touches credentials or sessions.

use std::io::Read;
use std::time::Duration;

const MAX_BODY: u64 = 512 * 1024;
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

/// Fetch the page title for `url`. Errors are plain strings for the UI.
pub fn fetch_page_title(url: &str) -> Result<String, String> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("URL must start with http:// or https://.".to_string());
    }
    let mut body = Vec::new();
    ureq::get(url)
        .set("User-Agent", UA)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("Could not load the page: {e}"))?
        .into_reader()
        .take(MAX_BODY)
        .read_to_end(&mut body)
        .map_err(|e| format!("Could not read the page: {e}"))?;
    let html = String::from_utf8_lossy(&body);
    extract_title(&html).ok_or_else(|| "No page title found.".to_string())
}

/// Case-insensitive `<title>…</title>` extraction: whitespace collapsed and
/// the common HTML entities decoded. Returns None when there is no usable
/// title.
fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let open = lower.find("<title")?;
    let content_start = open + lower[open..].find('>')? + 1;
    let content_end = content_start + lower[content_start..].find("</title>")?;
    let raw = html.get(content_start..content_end)?.trim();
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(
        collapsed
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&nbsp;", " "),
    )
}

#[cfg(test)]
mod tests {
    use super::extract_title;

    #[test]
    fn extracts_simple_title() {
        let html = "<html><head><title>  Hello&nbsp;World </title></head></html>";
        assert_eq!(extract_title(html).as_deref(), Some("Hello World"));
    }

    #[test]
    fn case_insensitive_with_entities_and_attrs() {
        let html = "<HTML><HEAD><TITLE class=\"x\">Fish &amp; Chips</TITLE></HEAD>";
        assert_eq!(extract_title(html).as_deref(), Some("Fish & Chips"));
    }

    #[test]
    fn missing_title_is_none() {
        assert_eq!(extract_title("<html><body>nope</body></html>"), None);
    }
}
