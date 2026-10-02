//! Pure `<link rel="icon">` parsing for favicon discovery.
//!
//! Kept free of Tauri/store dependencies on purpose: the parsing is the
//! fiddliest part of favicon fetching, and this module's unit tests are the
//! proof it works. The HTTP download + caching lives in `favicon.rs`.

/// What kind of icon a `<link>` tag declares. Ordering matters for
/// preference: `apple-touch-icon` is usually a large real PNG, a plain
/// `icon` is next, `shortcut icon` is the legacy fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    AppleTouch,
    Icon,
    Shortcut,
}

/// One usable icon candidate found in the page HTML.
#[derive(Debug, Clone)]
pub struct IconCandidate {
    /// Absolute URL, resolved against the page URL.
    pub href: String,
    /// Largest declared dimension in px; 0 when the tag declares no sizes.
    pub max_size: u32,
    pub kind: IconKind,
}

fn kind_rank(kind: IconKind) -> u8 {
    match kind {
        IconKind::AppleTouch => 0,
        IconKind::Icon => 1,
        IconKind::Shortcut => 2,
    }
}

/// Pick the best candidate: apple-touch-icon first, then plain icon, then
/// shortcut icon; within a kind, larger declared sizes win over unknown.
pub fn choose_icon(candidates: &[IconCandidate]) -> Option<&IconCandidate> {
    candidates.iter().max_by(|a, b| {
        kind_rank(b.kind)
            .cmp(&kind_rank(a.kind))
            .then_with(|| a.max_size.cmp(&b.max_size))
    })
}

/// Find `<link …>` tag spans (byte ranges) in `html`, case-insensitively.
/// The scan respects quoted attribute values so a `>` inside quotes (e.g. in
/// a data: URL) doesn't end the tag early.
fn link_tag_spans(html: &str) -> Vec<(usize, usize)> {
    let lower = html.to_lowercase();
    let bytes = html.as_bytes();
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(rel) = lower[pos..].find("<link") {
        let start = pos + rel;
        // "link" must be followed by whitespace, '/' or '>' — not "<linkfoo".
        let valid = matches!(
            lower.as_bytes().get(start + 5),
            Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') | Some(b'/') | Some(b'>')
        );
        if !valid {
            pos = start + 5;
            continue;
        }
        let mut i = start + 5;
        let mut quote: Option<u8> = None;
        let mut end: Option<usize> = None;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = quote {
                if b == q {
                    quote = None;
                }
            } else if b == b'"' || b == b'\'' {
                quote = Some(b);
            } else if b == b'>' {
                end = Some(i);
                break;
            }
            i += 1;
        }
        match end {
            Some(e) => {
                spans.push((start, e + 1));
                pos = e + 1;
            }
            None => break,
        }
    }
    spans
}

/// Parse `name="value"` / `name='value'` / `name=value` pairs from a tag's
/// inner text. Attribute names are lowercased; the tag name itself is
/// skipped.
fn parse_attrs(tag: &str) -> Vec<(String, String)> {
    let bytes = tag.as_bytes();
    let mut attrs = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'>' {
            break;
        }
        let name_start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'-' | b'_' | b':'))
        {
            i += 1;
        }
        if name_start == i {
            i += 1;
            continue;
        }
        let name = tag[name_start..i].to_lowercase();
        // The first token is the tag name ("link"), not an attribute.
        if name == "link" {
            continue;
        }
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let q = bytes[i];
                i += 1;
                let vstart = i;
                while i < bytes.len() && bytes[i] != q {
                    i += 1;
                }
                value = tag[vstart..i].to_string();
                if i < bytes.len() {
                    i += 1;
                }
            } else {
                let vstart = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = tag[vstart..i].to_string();
            }
        }
        attrs.push((name, value));
    }
    attrs
}

/// `"16x16 32x32"` → 32; `"any"` → u32::MAX; unparseable → 0.
fn parse_sizes(s: &str) -> u32 {
    let mut best = 0u32;
    for token in s.split_whitespace() {
        let t = token.to_lowercase();
        if t == "any" {
            return u32::MAX;
        }
        let mut parts = t.split('x');
        let w = parts.next().and_then(|p| p.parse::<u32>().ok()).unwrap_or(0);
        let h = parts.next().and_then(|p| p.parse::<u32>().ok()).unwrap_or(0);
        best = best.max(w.max(h));
    }
    best
}

/// Collect icon candidates from the page HTML, resolving relative hrefs
/// against `base`. Skips tags without rel/href and data: URLs (decoding
/// those is not worth a base64 dependency for a best-effort logo).
pub fn parse_icon_candidates(html: &str, base: &url::Url) -> Vec<IconCandidate> {
    let mut out = Vec::new();
    for (start, end) in link_tag_spans(html) {
        let attrs = parse_attrs(&html[start..end]);
        let get = |n: &str| attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
        let rel = match get("rel") {
            Some(r) if !r.trim().is_empty() => r,
            _ => continue,
        };
        let rel_lower = rel.to_lowercase();
        let tokens: Vec<&str> = rel_lower.split_whitespace().collect();
        let kind = if tokens.iter().any(|t| t.starts_with("apple-touch-icon")) {
            IconKind::AppleTouch
        } else if tokens.contains(&"icon") {
            if tokens.contains(&"shortcut") {
                IconKind::Shortcut
            } else {
                IconKind::Icon
            }
        } else {
            continue;
        };
        let href = match get("href") {
            Some(h) if !h.trim().is_empty() => h.trim(),
            _ => continue,
        };
        if href.to_lowercase().starts_with("data:") {
            continue;
        }
        let resolved = match base.join(href) {
            Ok(u) => u.to_string(),
            Err(_) => continue,
        };
        let max_size = get("sizes").map(parse_sizes).unwrap_or(0);
        out.push(IconCandidate {
            href: resolved,
            max_size,
            kind,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> url::Url {
        url::Url::parse("https://example.com/page").unwrap()
    }

    #[test]
    fn prefers_largest_apple_touch_icon() {
        let html = r#"<head>
            <link rel="icon" href="/small.png" sizes="16x16">
            <link rel="apple-touch-icon" href="/touch-120.png" sizes="120x120">
            <link rel="apple-touch-icon" href="/touch-180.png" sizes="180x180">
        </head>"#;
        let c = parse_icon_candidates(html, &base());
        let best = choose_icon(&c).unwrap();
        assert_eq!(best.href, "https://example.com/touch-180.png");
        assert_eq!(best.kind, IconKind::AppleTouch);
    }

    #[test]
    fn icon_beats_shortcut_icon() {
        let html = r#"<head>
            <link rel="shortcut icon" href="/old.ico">
            <link rel="icon" href="/new.png">
        </head>"#;
        let c = parse_icon_candidates(html, &base());
        let best = choose_icon(&c).unwrap();
        assert_eq!(best.href, "https://example.com/new.png");
    }

    #[test]
    fn resolves_relative_hrefs_and_ignores_data_urls() {
        let html = r#"<head>
            <link rel="icon" href="data:image/png;base64,AAAA">
            <link REL="ICON" HREF="assets/fav.png" SIZES="32x32">
        </head>"#;
        let c = parse_icon_candidates(html, &base());
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].href, "https://example.com/assets/fav.png");
        assert_eq!(c[0].max_size, 32);
    }

    #[test]
    fn quoted_gt_does_not_end_tag_early() {
        let html = r#"<head><link rel="icon" href="/a.png" title="a > b" sizes="48x48"></head>"#;
        let c = parse_icon_candidates(html, &base());
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].max_size, 48);
    }

    #[test]
    fn sizes_any_counts_as_huge() {
        assert_eq!(parse_sizes("any"), u32::MAX);
        assert_eq!(parse_sizes("16x16 32x32"), 32);
        assert_eq!(parse_sizes("bogus"), 0);
    }

    #[test]
    fn no_candidates_is_none() {
        let html = "<html><head><title>No icons</title></head></html>";
        let c = parse_icon_candidates(html, &base());
        assert!(choose_icon(&c).is_none());
    }
}
