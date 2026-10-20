//! When a certificate ends: the `notAfter` date of an X.509 certificate, read from its DER form without a full X.509 parser.
//!
//! A certificate is `SEQUENCE { tbsCertificate SEQUENCE { [0] version OPTIONAL, serialNumber, signature, issuer, validity SEQUENCE { notBefore,
//! notAfter }, … }, … }` (RFC 5280, section 4.1). Anything that doesn't have this shape gives `None`.

/// One DER element: its tag and content, and what follows it
struct Element<'a> {
    tag: u8,
    content: &'a [u8],
    rest: &'a [u8],
}

const SEQUENCE: u8 = 0x30;
const VERSION: u8 = 0xa0;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;

/// The element at the start of `input`. Only single-byte tags and definite lengths of up to four bytes (what DER allows) are read
fn element(input: &[u8]) -> Option<Element<'_>> {
    let (&tag, input) = input.split_first()?;
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first, input) = input.split_first()?;
    let (length, input) = if first < 0x80 {
        (usize::from(first), input)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || input.len() < count {
            return None;
        }
        let (bytes, input) = input.split_at(count);
        (bytes.iter().fold(0usize, |length, &byte| (length << 8) | usize::from(byte)), input)
    };
    if length > input.len() {
        return None;
    }
    let (content, rest) = input.split_at(length);
    Some(Element { tag, content, rest })
}

/// The element at the start of `input`, when it has this tag
fn expect(input: &[u8], tag: u8) -> Option<Element<'_>> {
    element(input).filter(|e| e.tag == tag)
}

/// A `UTCTime` (`YYMMDDHHMMSSZ`, 1950 to 2049) or `GeneralizedTime` (`YYYYMMDDHHMMSSZ`) in UTC, as Unix seconds
fn time(e: &Element) -> Option<i64> {
    let text = std::str::from_utf8(e.content).ok()?;
    let digits = text.strip_suffix('Z')?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let pair = |at: usize| digits.get(at..at + 2)?.parse::<i8>().ok();
    let (year, at) = match (e.tag, digits.len()) {
        (UTC_TIME, 12) => {
            let year = i16::from(pair(0)?);
            (if year >= 50 { 1900 + year } else { 2000 + year }, 2)
        }
        (GENERALIZED_TIME, 14) => (i16::from(pair(0)?) * 100 + i16::from(pair(2)?), 4),
        _ => return None,
    };
    let date = jiff::civil::DateTime::new(year, pair(at)?, pair(at + 2)?, pair(at + 4)?, pair(at + 6)?, pair(at + 8)?, 0).ok()?;
    Some(date.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp().as_second())
}

/// When the certificate in `der` ends (Unix seconds)
pub(super) fn not_after(der: &[u8]) -> Option<i64> {
    let certificate = expect(der, SEQUENCE)?;
    let mut fields = expect(certificate.content, SEQUENCE)?.content;
    // The version is there from version 2 on; then the serial number, the signature algorithm and the issuer
    if let Some(version) = element(fields).filter(|e| e.tag == VERSION) {
        fields = version.rest;
    }
    for _ in 0..3 {
        fields = element(fields)?.rest;
    }
    let validity = expect(fields, SEQUENCE)?;
    let not_before = element(validity.content)?;
    time(&element(not_before.rest)?)
}

#[cfg(test)]
mod tests;
