//! LDAP search filters (RFC 4515): the text an administrator writes, with the user name put in escaped, read into a
//! tree and written as BER for the search request.

use super::ber::{self, CONSTRUCTED, CONTEXT};

/// Deepest nesting and longest text of a filter that is taken
const MAX_DEPTH: usize = 16;
const MAX_LENGTH: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
    Equal(String, Vec<u8>),
    Approx(String, Vec<u8>),
    GreaterOrEqual(String, Vec<u8>),
    LessOrEqual(String, Vec<u8>),
    Present(String),
    Substrings {
        attr: String,
        initial: Option<Vec<u8>>,
        any: Vec<Vec<u8>>,
        last: Option<Vec<u8>>,
    },
    /// `attr:rule:=value` (`dn_attrs`: `:dn`); the rule or the attribute may be left out
    Extensible {
        rule: Option<String>,
        attr: Option<String>,
        value: Vec<u8>,
        dn_attrs: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub &'static str);

/// A value written into a filter, so that it can only be a value: `*` `(` `)` `\`, NUL, control characters and anything
/// that isn't ASCII are written as `\xx` (RFC 4515 §3). A user name like `*)(uid=*` finds nobody.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if !(0x20..0x7f).contains(&b) || matches!(b, b'*' | b'(' | b')' | b'\\') {
            out.push_str(&format!("\\{b:02x}"));
        } else {
            out.push(b as char);
        }
    }
    out
}

/// The filter an administrator's text makes for `username`: every `{username}` is replaced by the escaped name. The
/// text has to have it somewhere (a filter that doesn't ask for the person would find everyone).
pub fn for_user(template: &str, username: &str) -> Result<Filter, Invalid> {
    if !template.contains("{username}") {
        return Err(Invalid("The filter has to contain {username}"));
    }
    parse(&template.replace("{username}", &escape(username)))
}

/// The filter of a text; a value in it may have `\xx` escapes
pub fn parse(text: &str) -> Result<Filter, Invalid> {
    if text.len() > MAX_LENGTH {
        return Err(Invalid("The filter is too long"));
    }
    let bytes = text.trim().as_bytes();
    let (filter, rest) = filter(bytes, 0)?;
    if !rest.is_empty() {
        return Err(Invalid("Something follows the filter's last bracket"));
    }
    Ok(filter)
}

fn filter(input: &[u8], depth: usize) -> Result<(Filter, &[u8]), Invalid> {
    if depth > MAX_DEPTH {
        return Err(Invalid("The filter is nested too deeply"));
    }
    let rest = input.strip_prefix(b"(").ok_or(Invalid("A filter starts with ("))?;
    let (inner, rest) = match rest.first() {
        Some(b'&') | Some(b'|') => {
            let op = rest[0];
            let mut rest = &rest[1..];
            let mut parts = Vec::new();
            while rest.first() == Some(&b'(') {
                let (part, after) = filter(rest, depth + 1)?;
                parts.push(part);
                rest = after;
            }
            if parts.is_empty() {
                return Err(Invalid("& and | need at least one filter inside"));
            }
            (if op == b'&' { Filter::And(parts) } else { Filter::Or(parts) }, rest)
        }
        Some(b'!') => {
            let (part, rest) = filter(&rest[1..], depth + 1)?;
            (Filter::Not(Box::new(part)), rest)
        }
        _ => {
            let end = rest.iter().position(|b| *b == b')').ok_or(Invalid("A bracket is not closed"))?;
            (item(&rest[..end])?, &rest[end..])
        }
    };
    let rest = rest.strip_prefix(b")").ok_or(Invalid("A bracket is not closed"))?;
    Ok((inner, rest))
}

fn is_attr_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b';' | b'_')
}

/// One comparison, without its brackets
fn item(text: &[u8]) -> Result<Filter, Invalid> {
    let eq = text.iter().position(|b| *b == b'=').ok_or(Invalid("A comparison needs ="))?;
    let (left, value) = (&text[..eq], &text[eq + 1..]);
    // The kind of comparison is the last character before =
    let (kind, left) = match left.last() {
        Some(b'~') => (b'~', &left[..left.len() - 1]),
        Some(b'>') => (b'>', &left[..left.len() - 1]),
        Some(b'<') => (b'<', &left[..left.len() - 1]),
        Some(b':') => (b':', &left[..left.len() - 1]),
        _ => (b'=', left),
    };
    if kind == b':' {
        return extensible(left, value);
    }
    if left.is_empty() || !left.iter().copied().all(is_attr_char) {
        return Err(Invalid("The attribute name has characters it can't have"));
    }
    let attr = String::from_utf8_lossy(left).into_owned();
    match kind {
        b'~' => Ok(Filter::Approx(attr, unescape(value)?)),
        b'>' => Ok(Filter::GreaterOrEqual(attr, unescape(value)?)),
        b'<' => Ok(Filter::LessOrEqual(attr, unescape(value)?)),
        _ if value == b"*" => Ok(Filter::Present(attr)),
        _ if value.contains(&b'*') => {
            let mut parts = value.split(|b| *b == b'*');
            let initial = parts.next().filter(|p| !p.is_empty()).map(unescape).transpose()?;
            let mut pieces: Vec<&[u8]> = parts.collect();
            let last = pieces.pop().filter(|p| !p.is_empty()).map(unescape).transpose()?;
            let any = pieces.into_iter().filter(|p| !p.is_empty()).map(unescape).collect::<Result<Vec<_>, _>>()?;
            Ok(Filter::Substrings { attr, initial, any, last })
        }
        _ => Ok(Filter::Equal(attr, unescape(value)?)),
    }
}

/// `attr[:dn][:rule]:=value` (the `:` before `=` is already taken off `left`)
fn extensible(left: &[u8], value: &[u8]) -> Result<Filter, Invalid> {
    let text = std::str::from_utf8(left).map_err(|_| Invalid("The matching rule has characters it can't have"))?;
    let mut parts = text.split(':');
    let attr = parts.next().filter(|a| !a.is_empty()).map(str::to_string);
    if attr.as_deref().is_some_and(|a| !a.bytes().all(is_attr_char)) {
        return Err(Invalid("The attribute name has characters it can't have"));
    }
    let (mut dn_attrs, mut rule) = (false, None);
    for p in parts {
        if p.eq_ignore_ascii_case("dn") && !dn_attrs && rule.is_none() {
            dn_attrs = true;
        } else if rule.is_none() && !p.is_empty() && !p.eq_ignore_ascii_case("dn") && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
            rule = Some(p.to_string());
        } else {
            return Err(Invalid("The matching rule is not written as :dn:rule:="));
        }
    }
    if attr.is_none() && rule.is_none() {
        return Err(Invalid("An extensible match needs an attribute or a rule"));
    }
    Ok(Filter::Extensible { rule, attr, value: unescape(value)?, dn_attrs })
}

/// The bytes a value stands for: `\xx` is one byte
fn unescape(value: &[u8]) -> Result<Vec<u8>, Invalid> {
    let mut out = Vec::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        if value[i] == b'\\' {
            let hex = value.get(i + 1..i + 3).ok_or(Invalid("A \\ has to be followed by two hexadecimal digits"))?;
            let hex =
                std::str::from_utf8(hex).ok().and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or(Invalid("A \\ has to be followed by two hexadecimal digits"))?;
            out.push(hex);
            i += 3;
        } else if matches!(value[i], b'(' | b')') {
            return Err(Invalid("A bracket inside a value is written \\28 or \\29"));
        } else {
            out.push(value[i]);
            i += 1;
        }
    }
    Ok(out)
}

// ───────────── As BER ─────────────

impl Filter {
    pub fn to_ber(&self) -> Vec<u8> {
        let pair =
            |tag: u8, attr: &str, value: &[u8]| ber::constructed(tag, &[ber::octets(ber::OCTET_STRING, attr.as_bytes()), ber::octets(ber::OCTET_STRING, value)]);
        match self {
            Filter::And(parts) => ber::constructed(CONTEXT | CONSTRUCTED, &parts.iter().map(Filter::to_ber).collect::<Vec<_>>()),
            Filter::Or(parts) => ber::constructed(CONTEXT | CONSTRUCTED | 1, &parts.iter().map(Filter::to_ber).collect::<Vec<_>>()),
            Filter::Not(part) => ber::constructed(CONTEXT | CONSTRUCTED | 2, &[part.to_ber()]),
            Filter::Equal(a, v) => pair(CONTEXT | CONSTRUCTED | 3, a, v),
            Filter::GreaterOrEqual(a, v) => pair(CONTEXT | CONSTRUCTED | 5, a, v),
            Filter::LessOrEqual(a, v) => pair(CONTEXT | CONSTRUCTED | 6, a, v),
            Filter::Approx(a, v) => pair(CONTEXT | CONSTRUCTED | 8, a, v),
            Filter::Present(a) => ber::octets(CONTEXT | 7, a.as_bytes()),
            Filter::Substrings { attr, initial, any, last } => {
                let mut subs = Vec::new();
                subs.extend(initial.iter().map(|v| ber::octets(CONTEXT, v)));
                subs.extend(any.iter().map(|v| ber::octets(CONTEXT | 1, v)));
                subs.extend(last.iter().map(|v| ber::octets(CONTEXT | 2, v)));
                ber::constructed(CONTEXT | CONSTRUCTED | 4, &[ber::octets(ber::OCTET_STRING, attr.as_bytes()), ber::constructed(ber::SEQUENCE, &subs)])
            }
            Filter::Extensible { rule, attr, value, dn_attrs } => {
                let mut parts = Vec::new();
                parts.extend(rule.iter().map(|r| ber::octets(CONTEXT | 1, r.as_bytes())));
                parts.extend(attr.iter().map(|a| ber::octets(CONTEXT | 2, a.as_bytes())));
                parts.push(ber::octets(CONTEXT | 3, value));
                if *dn_attrs {
                    parts.push(ber::boolean(CONTEXT | 4, true));
                }
                ber::constructed(CONTEXT | CONSTRUCTED | 9, &parts)
            }
        }
    }

    /// A filter read back from BER (a server of tests reads what the client sent)
    #[cfg(test)]
    pub fn from_ber(tlv: ber::Tlv<'_>) -> Result<Filter, ber::Error> {
        let text = |t: &ber::Tlv<'_>| t.text();
        let pair = |t: &ber::Tlv<'_>| -> Result<(String, Vec<u8>), ber::Error> {
            let parts = ber::items(t.content)?;
            match parts.as_slice() {
                [a, v] => Ok((text(a), v.content.to_vec())),
                _ => Err(ber::Error::Malformed),
            }
        };
        let list = |t: &ber::Tlv<'_>| -> Result<Vec<Filter>, ber::Error> { ber::items(t.content)?.into_iter().map(Filter::from_ber).collect() };
        Ok(match tlv.tag {
            t if t == CONTEXT | CONSTRUCTED => Filter::And(list(&tlv)?),
            t if t == CONTEXT | CONSTRUCTED | 1 => Filter::Or(list(&tlv)?),
            t if t == CONTEXT | CONSTRUCTED | 2 => Filter::Not(Box::new(Filter::from_ber(ber::read(tlv.content)?.0)?)),
            t if t == CONTEXT | CONSTRUCTED | 3 => {
                let (a, v) = pair(&tlv)?;
                Filter::Equal(a, v)
            }
            t if t == CONTEXT | CONSTRUCTED | 5 => {
                let (a, v) = pair(&tlv)?;
                Filter::GreaterOrEqual(a, v)
            }
            t if t == CONTEXT | CONSTRUCTED | 6 => {
                let (a, v) = pair(&tlv)?;
                Filter::LessOrEqual(a, v)
            }
            t if t == CONTEXT | CONSTRUCTED | 8 => {
                let (a, v) = pair(&tlv)?;
                Filter::Approx(a, v)
            }
            t if t == CONTEXT | 7 => Filter::Present(text(&tlv)),
            t if t == CONTEXT | CONSTRUCTED | 4 => {
                let parts = ber::items(tlv.content)?;
                let [attr, subs] = parts.as_slice() else { return Err(ber::Error::Malformed) };
                let (mut initial, mut any, mut last) = (None, Vec::new(), None);
                for s in ber::items(subs.content)? {
                    match s.tag {
                        t if t == CONTEXT => initial = Some(s.content.to_vec()),
                        t if t == CONTEXT | 1 => any.push(s.content.to_vec()),
                        t if t == CONTEXT | 2 => last = Some(s.content.to_vec()),
                        _ => return Err(ber::Error::Malformed),
                    }
                }
                Filter::Substrings { attr: text(attr), initial, any, last }
            }
            t if t == CONTEXT | CONSTRUCTED | 9 => {
                let (mut rule, mut attr, mut value, mut dn_attrs) = (None, None, Vec::new(), false);
                for p in ber::items(tlv.content)? {
                    match p.tag {
                        t if t == CONTEXT | 1 => rule = Some(text(&p)),
                        t if t == CONTEXT | 2 => attr = Some(text(&p)),
                        t if t == CONTEXT | 3 => value = p.content.to_vec(),
                        t if t == CONTEXT | 4 => dn_attrs = p.boolean()?,
                        _ => return Err(ber::Error::Malformed),
                    }
                }
                Filter::Extensible { rule, attr, value, dn_attrs }
            }
            _ => return Err(ber::Error::Malformed),
        })
    }
}

#[cfg(test)]
mod tests;
