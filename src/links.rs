//! Link extraction ported from Go `internal/workspace/links.go`.
//!
//! The chat draws a card for up to two links of a message. URLs inside code
//! spans, trailing sentence punctuation, userinfo and unbalanced closing
//! brackets are handled exactly like the Go client so the same text produces
//! the same cards.

use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextLink {
    /// Rune index of the first character in the message.
    pub start: usize,
    /// Rune index after the last character.
    pub end: usize,
    /// The exact address opened by a click: explicit `http://` and a `#fragment`
    /// stay as written; a bare domain becomes `https://`.
    pub href: String,
}

/// One preview card. The cache key is normalized (HTTPS, no fragment) while a
/// click opens the first original address of that key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewLink {
    pub cache_url: String,
    pub open_href: String,
}

/// Scan a message body. Returns the clickable links and up to two preview
/// keys, each with the address the card opens.
pub fn scan(content: &str) -> (Vec<TextLink>, Vec<PreviewLink>) {
    let chars: Vec<char> = content.chars().collect();
    let code = code_spans(&chars);
    let mut links = Vec::new();
    let mut previews: Vec<PreviewLink> = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let Some(len) = candidate_len(&chars, index) else {
            index += 1;
            continue;
        };
        let end = trim_end(&chars[index..index + len]) + index;
        if end <= index {
            index += 1;
            continue;
        }
        if code
            .iter()
            .any(|(start, stop)| index >= *start && index < *stop)
        {
            index = end;
            continue;
        }
        let value: String = chars[index..end].iter().collect();
        if let Some((open_href, cache_url)) = normalize(&value) {
            links.push(TextLink {
                start: index,
                end,
                href: open_href.clone(),
            });
            if previews.len() < 2
                && previews.first().map(|item| &item.cache_url) != Some(&cache_url)
            {
                previews.push(PreviewLink {
                    cache_url,
                    open_href,
                });
            }
        }
        index = end;
    }
    (links, previews)
}

/// Go `codePattern`: fenced blocks first, then inline spans without a newline.
fn code_spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '`'
            && index + 2 < chars.len()
            && chars[index + 1] == '`'
            && chars[index + 2] == '`'
        {
            let mut stop = index + 3;
            while stop + 2 < chars.len()
                && !(chars[stop] == '`' && chars[stop + 1] == '`' && chars[stop + 2] == '`')
            {
                stop += 1;
            }
            let end = (stop + 3).min(chars.len());
            spans.push((index, end));
            index = end;
            continue;
        }
        if chars[index] == '`' {
            let mut stop = index + 1;
            while stop < chars.len() && chars[stop] != '`' && chars[stop] != '\n' {
                stop += 1;
            }
            if stop < chars.len() && chars[stop] == '`' {
                spans.push((index, stop + 1));
                index = stop + 1;
                continue;
            }
        }
        index += 1;
    }
    spans
}

/// Length in chars of a URL or a bare domain starting at `start`.
fn candidate_len(chars: &[char], start: usize) -> Option<usize> {
    let rest: String = chars[start..].iter().take(8).collect();
    let lower = rest.to_ascii_lowercase();
    let head = if lower.starts_with("https://") {
        8
    } else if lower.starts_with("http://") {
        7
    } else {
        return bare_domain_len(chars, start);
    };
    let mut end = start + head;
    while end < chars.len() && is_url_char(chars[end]) {
        end += 1;
    }
    // The host must not be empty: `https://` alone is not a link.
    (end > start + head + 1).then_some(end - start)
}

fn bare_domain_len(chars: &[char], start: usize) -> Option<usize> {
    if start > 0 {
        let previous = chars[start - 1];
        if previous.is_alphanumeric()
            || matches!(previous, '_' | '@' | '.' | '/' | ':' | '\\' | '-')
        {
            return None;
        }
    }
    let mut end = start;
    let mut dots = 0;
    loop {
        let label_start = end;
        while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '-') {
            end += 1;
        }
        if end == label_start {
            return None;
        }
        if end < chars.len() && chars[end] == '.' {
            dots += 1;
            end += 1;
            continue;
        }
        break;
    }
    if dots == 0 {
        return None;
    }
    // A top-level label of one character is not a domain (`a.b` is).
    let last_label_start = chars[start..end]
        .iter()
        .rposition(|ch| *ch == '.')
        .map(|position| start + position + 1)?;
    if end - last_label_start < 2 {
        return None;
    }
    if end < chars.len() && chars[end] == ':' {
        let mut port = end + 1;
        while port < chars.len() && chars[port].is_ascii_digit() {
            port += 1;
        }
        if port > end + 1 && port - end - 1 <= 5 {
            end = port;
        }
    }
    if end < chars.len() && matches!(chars[end], '/' | '?' | '#') {
        while end < chars.len() && is_url_char(chars[end]) {
            end += 1;
        }
    }
    (end > start).then_some(end - start)
}

fn is_url_char(ch: char) -> bool {
    !ch.is_whitespace()
        && !ch.is_control()
        && !matches!(ch, '<' | '>' | '`' | '"' | '\'' | '«' | '»')
}

/// Go trailing punctuation rule, including balanced brackets.
fn trim_end(value: &[char]) -> usize {
    let mut end = value.len();
    loop {
        if end == 0 {
            return end;
        }
        let last = value[end - 1];
        if matches!(last, '.' | ',' | '!' | '?' | ':' | ';' | '»' | '”') {
            end -= 1;
            continue;
        }
        let open = match last {
            ')' => Some('('),
            ']' => Some('['),
            '}' => Some('{'),
            _ => None,
        };
        let Some(open) = open else {
            return end;
        };
        let closing = value[..end].iter().filter(|ch| **ch == last).count();
        let opening = value[..end].iter().filter(|ch| **ch == open).count();
        if closing > opening {
            end -= 1;
            continue;
        }
        return end;
    }
}

/// Go normalization: http(s) only, no userinfo, `https` scheme, no fragment
/// and an explicit root path.
/// Go normalization: `(open, cache)`. The click keeps the original scheme and
/// fragment; the preview key is HTTPS without a fragment and with a root path.
fn normalize(value: &str) -> Option<(String, String)> {
    let target = if value
        .get(..7)
        .is_some_and(|head| head.eq_ignore_ascii_case("http://"))
        || value
            .get(..8)
            .is_some_and(|head| head.eq_ignore_ascii_case("https://"))
    {
        value.to_string()
    } else {
        format!("https://{value}")
    };
    let url = Url::parse(&target).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.host_str().is_none_or(str::is_empty) {
        return None;
    }
    if url.scheme() != "https" && url.scheme() != "http" {
        return None;
    }
    let open_href = url.to_string();
    let mut cache = url;
    cache.set_scheme("https").ok()?;
    cache.set_fragment(None);
    if cache.path().is_empty() {
        cache.set_path("/");
    }
    Some((open_href, cache.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hrefs(content: &str) -> Vec<String> {
        scan(content).0.into_iter().map(|link| link.href).collect()
    }

    #[test]
    fn click_links_keep_their_original_scheme_and_fragment() {
        assert_eq!(
            hrefs("смотри https://example.com/a."),
            ["https://example.com/a"]
        );
        assert_eq!(hrefs("example.com/path"), ["https://example.com/path"]);
        assert_eq!(
            hrefs("https://example.com/page#section"),
            ["https://example.com/page#section"]
        );
        assert_eq!(hrefs("https://example.com"), ["https://example.com/"]);
        assert_eq!(
            hrefs("http://127.0.0.1:8080/docs#setup"),
            ["http://127.0.0.1:8080/docs#setup"]
        );
    }

    #[test]
    fn code_spans_are_ignored() {
        assert!(hrefs("`https://example.com`").is_empty());
        assert!(hrefs("```\nhttps://example.com\n```").is_empty());
        assert_eq!(
            hrefs("`code` and https://example.com"),
            ["https://example.com/"]
        );
    }

    #[test]
    fn trailing_punctuation_and_brackets_follow_go() {
        assert_eq!(hrefs("(https://example.com/a)"), ["https://example.com/a"]);
        assert_eq!(
            hrefs("https://en.wikipedia.org/wiki/Rust_(programming_language)"),
            ["https://en.wikipedia.org/wiki/Rust_(programming_language)"]
        );
        assert_eq!(hrefs("https://example.com/a»"), ["https://example.com/a"]);
    }

    #[test]
    fn word_boundaries_and_invalid_targets_are_skipped() {
        assert!(hrefs("x_example.com").is_empty());
        assert!(hrefs("@example.com").is_empty());
        assert!(hrefs("https://user:pass@example.com").is_empty());
        assert_eq!(
            hrefs("http://localhost:8080/health"),
            ["http://localhost:8080/health"]
        );
    }

    #[test]
    fn preview_keys_are_https_without_a_fragment() {
        let (links, previews) = scan("http://127.0.0.1:8080/docs#setup");
        assert_eq!(links[0].href, "http://127.0.0.1:8080/docs#setup");
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].cache_url, "https://127.0.0.1:8080/docs");
        assert_eq!(previews[0].open_href, "http://127.0.0.1:8080/docs#setup");
    }

    #[test]
    fn two_fragments_share_one_card_but_keep_two_click_addresses() {
        let (links, previews) = scan("https://example.com/a#one and https://example.com/a#two");
        assert_eq!(links.len(), 2);
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].cache_url, "https://example.com/a");
        assert_eq!(previews[0].open_href, "https://example.com/a#one");
        assert_eq!(links[0].href, "https://example.com/a#one");
        assert_eq!(links[1].href, "https://example.com/a#two");
    }

    #[test]
    fn at_most_two_unique_previews_per_message() {
        let (links, previews) = scan(
            "https://one.example.com https://one.example.com https://two.example.com https://three.example.com",
        );
        assert_eq!(links.len(), 4);
        let cache: Vec<_> = previews.iter().map(|p| p.cache_url.clone()).collect();
        assert_eq!(
            cache,
            ["https://one.example.com/", "https://two.example.com/"]
        );
    }

    #[test]
    fn rune_indices_point_into_the_message() {
        let content = "привет https://example.com!";
        let (links, _) = scan(content);
        assert_eq!(links.len(), 1);
        let chars: Vec<char> = content.chars().collect();
        assert_eq!(
            chars[links[0].start..links[0].end]
                .iter()
                .collect::<String>(),
            "https://example.com"
        );
    }
}
