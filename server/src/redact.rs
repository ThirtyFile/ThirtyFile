//! Leaving out of a message what could name something in a space or let someone in (for the error log)

/// Keys whose values are secrets wherever they appear as `key=value`
const SECRET_KEYS: [&str; 10] = ["token", "password", "passwd", "secret", "key", "code", "ticket", "sig", "session", "auth"];

/// Leaves out of a message what could name something in a space or let someone in: text in quotes ("…", “…”, ‘…’,
/// and 「…」 or 『…』 as Chinese text quotes names), the values of secret-looking `key=value` pairs, share link tokens and `Bearer` credentials
pub fn redact(s: &str) -> String {
    let quoted = redact_quotes(s);
    let mut out = String::with_capacity(quoted.len());
    let mut hide_next = false;
    for part in quoted.split_inclusive(|c: char| c.is_whitespace() || matches!(c, '&' | '?' | ';' | ',' | '(' | ')')) {
        let (word, sep) = match part.char_indices().last() {
            Some((at, c)) if c.is_whitespace() || matches!(c, '&' | '?' | ';' | ',' | '(' | ')') => (&part[..at], &part[at..]),
            _ => (part, ""),
        };
        if hide_next && !word.is_empty() {
            out.push('…');
            out.push_str(sep);
            hide_next = false;
            continue;
        }
        hide_next = word.eq_ignore_ascii_case("bearer") || word.eq_ignore_ascii_case("basic");
        out.push_str(&redact_word(word));
        out.push_str(sep);
    }
    out
}

/// A `key=value` whose key looks secret keeps its key only; a path's share token is left out
fn redact_word(word: &str) -> String {
    if let Some((key, _)) = word.split_once('=') {
        let k = key.rsplit(['/', '.', ':']).next().unwrap_or(key).to_ascii_lowercase();
        if SECRET_KEYS.iter().any(|s| k.contains(s)) {
            return format!("{key}=…");
        }
    }
    if word.contains("/share/") || word.contains("/shares/") {
        return page_route(word);
    }
    word.to_string()
}

fn redact_quotes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        let close = match c {
            '"' => '"',
            '“' => '”',
            '‘' => '’',
            // The quotes of Chinese and Japanese text
            '「' => '」',
            '『' => '』',
            _ => {
                out.push(c);
                continue;
            }
        };
        let rest = chars.as_str();
        match rest.find(close) {
            Some(end) => {
                out.push(c);
                out.push('…');
                out.push(close);
                chars = rest[end + close.len_utf8()..].chars();
            }
            None => out.push(c),
        }
    }
    out
}

/// At most `max` characters
pub fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

/// The page's path without its query, and without a share link's token
pub fn page_route(route: &str) -> String {
    let path = route.split(['?', '#']).next().unwrap_or_default();
    let mut out = Vec::new();
    let mut hide_next = false;
    for seg in path.split('/') {
        if hide_next && !seg.is_empty() {
            out.push("…");
            hide_next = false;
            continue;
        }
        hide_next = seg == "share" || seg == "shares";
        out.push(seg);
    }
    clip(&out.join("/"), 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_tokens_and_secrets_are_left_out() {
        assert_eq!(redact(r#"An item named "Salaries 2026.xlsx" already exists"#), r#"An item named "…" already exists"#);
        assert_eq!(redact("“Budget” changed while it was being moved"), "“…” changed while it was being moved");
        assert_eq!(redact("「Secret plan.docx」已存在"), "「…」已存在");
        assert_eq!(redact("無法把『病歷 2026』移到「醫療」"), "無法把『…』移到「…」");
        assert_eq!(redact("‘Diary.txt’ is locked, but it’s fine"), "‘…’ is locked, but it’s fine");
        assert_eq!(redact("GET /api/x?token=abc123&page=2 failed"), "GET /api/x?token=…&page=2 failed");
        assert_eq!(redact("password=hunter2, reset_key=xyz"), "password=…, reset_key=…");
        assert_eq!(redact("Authorization: Bearer abc.def"), "Authorization: Bearer …");
        assert_eq!(redact("at https://drive.example.com/share/AbCdEf/node"), "at https://drive.example.com/share/…/node");
        assert_eq!(redact("Cannot read properties of undefined (reading 'x')"), "Cannot read properties of undefined (reading 'x')");
        // An unclosed quote is left as it is
        assert_eq!(redact(r#"a "b"#), r#"a "b"#);
        assert_eq!(page_route("/share/secret-token/abc?x=1#y"), "/share/…/abc");
        assert_eq!(page_route("/files/abc?q=salaries"), "/files/abc");
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("數位檔案", 10), "數位檔案");
    }
}
