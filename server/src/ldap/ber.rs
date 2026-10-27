//! The part of ASN.1 BER that LDAP uses: definite lengths, tags below 31. Reading is strict and bounded (a hostile
//! server sends what it likes): a length longer than what is there, an indefinite length or a tag of the long form is
//! an error, never a panic or a large allocation.

/// The tag classes and the constructed bit, as they sit in a tag byte
pub const APPLICATION: u8 = 0x40;
pub const CONTEXT: u8 = 0x80;
pub const CONSTRUCTED: u8 = 0x20;

pub const BOOLEAN: u8 = 0x01;
pub const INTEGER: u8 = 0x02;
pub const OCTET_STRING: u8 = 0x04;
pub const ENUMERATED: u8 = 0x0a;
pub const SEQUENCE: u8 = 0x30;
#[cfg(test)]
pub const SET: u8 = 0x31;

/// Longest length this reader takes in a header (four bytes, as every LDAP message fits)
const MAX_LENGTH_BYTES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Cut short: more bytes were promised than there are
    Short,
    /// A form LDAP doesn't use (indefinite length, a long tag, a length of more than four bytes)
    Unsupported,
    /// An integer or boolean of the wrong size, or a value that is not what its place needs
    Malformed,
}

/// One element: its tag byte (class, constructed bit and number together) and its content
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub content: &'a [u8],
}

/// Reads the element at the start of `input`: it, and what follows it
pub fn read(input: &[u8]) -> Result<(Tlv<'_>, &[u8]), Error> {
    let (&tag, rest) = input.split_first().ok_or(Error::Short)?;
    if tag & 0x1f == 0x1f {
        return Err(Error::Unsupported);
    }
    let (&first, rest) = rest.split_first().ok_or(Error::Short)?;
    let (len, rest) = if first & 0x80 == 0 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > MAX_LENGTH_BYTES {
            return Err(Error::Unsupported);
        }
        let bytes = rest.get(..n).ok_or(Error::Short)?;
        (bytes.iter().fold(0usize, |acc, b| (acc << 8) | usize::from(*b)), &rest[n..])
    };
    let content = rest.get(..len).ok_or(Error::Short)?;
    Ok((Tlv { tag, content }, &rest[len..]))
}

/// The elements one after another that make up `content`
pub fn items(mut content: &[u8]) -> Result<Vec<Tlv<'_>>, Error> {
    let mut out = Vec::new();
    while !content.is_empty() {
        let (tlv, rest) = read(content)?;
        out.push(tlv);
        content = rest;
    }
    Ok(out)
}

impl Tlv<'_> {
    #[cfg(test)]
    pub fn is(&self, tag: u8) -> bool {
        self.tag == tag
    }

    /// An INTEGER or ENUMERATED as a number
    pub fn int(&self) -> Result<i64, Error> {
        if self.content.is_empty() || self.content.len() > 8 {
            return Err(Error::Malformed);
        }
        let negative = self.content[0] & 0x80 != 0;
        Ok(self.content.iter().fold(if negative { -1i64 } else { 0 }, |acc, b| (acc << 8) | i64::from(*b)))
    }

    #[cfg(test)]
    pub fn boolean(&self) -> Result<bool, Error> {
        match self.content {
            [b] => Ok(*b != 0),
            _ => Err(Error::Malformed),
        }
    }

    /// The text of an OCTET STRING (LDAP strings are UTF-8; what isn't is read lossily rather than refused: a directory
    /// holds what people typed into it)
    pub fn text(&self) -> String {
        String::from_utf8_lossy(self.content).into_owned()
    }
}

// ───────────── Writing ─────────────

fn length(len: usize) -> Vec<u8> {
    if len < 0x80 {
        vec![len as u8]
    } else {
        let bytes = len.to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        let mut out = vec![0x80 | (bytes.len() - skip) as u8];
        out.extend_from_slice(&bytes[skip..]);
        out
    }
}

/// An element with this tag byte and content
pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend(length(content.len()));
    out.extend_from_slice(content);
    out
}

pub fn int(tag: u8, value: i64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    // The shortest two's complement form
    let mut start = 0;
    while start < 7 && ((bytes[start] == 0 && bytes[start + 1] & 0x80 == 0) || (bytes[start] == 0xff && bytes[start + 1] & 0x80 != 0)) {
        start += 1;
    }
    tlv(tag, &bytes[start..])
}

pub fn boolean(tag: u8, value: bool) -> Vec<u8> {
    tlv(tag, &[if value { 0xff } else { 0 }])
}

pub fn octets(tag: u8, value: &[u8]) -> Vec<u8> {
    tlv(tag, value)
}

/// A constructed element made of the parts given
pub fn constructed(tag: u8, parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(tag, &parts.concat())
}

#[cfg(test)]
mod tests;
