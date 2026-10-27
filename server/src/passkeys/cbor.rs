//! A small, strict reader of CBOR (RFC 8949), for what WebAuthn sends: the attestation object, the authenticator data's
//! public key (a COSE key) and nothing else. It reads definite lengths of unsigned and negative integers, byte and text
//! strings, arrays, maps, and false, true and null. Everything else (tags, floats, indefinite lengths, a number too big
//! for 64 bits) is refused, and so is anything nested deeper or longer than a real authenticator sends, so a hostile
//! message can't use up memory or the stack.

/// How deep arrays and maps may nest
const MAX_DEPTH: usize = 8;
/// How many items (every value, key and element) one message may hold
const MAX_ITEMS: usize = 256;
/// The longest string or byte string: an RSA key or a certificate is the largest a real message carries
const MAX_STRING: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// An integer (unsigned up to 2^63 - 1, or negative down to -2^63)
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Value>),
    /// In the order they were written; keys are not required to be unique here (`get` finds the first)
    Map(Vec<(Value, Value)>),
    Bool(bool),
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The message ends before the value does
    Truncated,
    /// Bytes after the value
    Trailing,
    /// A kind of value this reader does not take (a tag, a float, an indefinite length, an integer past 64 bits)
    Unsupported,
    /// Nested deeper, longer or holding more than is allowed
    TooBig,
    /// Text that is not UTF-8
    Text,
}

impl Value {
    /// The value of the integer key `key` of a map
    pub fn get(&self, key: i64) -> Option<&Value> {
        let Value::Map(pairs) = self else { return None };
        pairs.iter().find(|(k, _)| *k == Value::Int(key)).map(|(_, v)| v)
    }

    /// The value of the text key `key` of a map
    pub fn get_text(&self, key: &str) -> Option<&Value> {
        let Value::Map(pairs) = self else { return None };
        pairs.iter().find(|(k, _)| matches!(k, Value::Text(t) if t == key)).map(|(_, v)| v)
    }

    pub fn as_int(&self) -> Option<i64> {
        if let Value::Int(i) = self { Some(*i) } else { None }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        if let Value::Bytes(b) = self { Some(b) } else { None }
    }

    pub fn as_text(&self) -> Option<&str> {
        if let Value::Text(t) = self { Some(t) } else { None }
    }
}

/// Reads one value that fills `bytes` exactly
pub fn parse(bytes: &[u8]) -> Result<Value, Error> {
    let (value, used) = parse_prefix(bytes)?;
    if used != bytes.len() {
        return Err(Error::Trailing);
    }
    Ok(value)
}

/// Reads the value at the start of `bytes`, and says how many bytes it took
pub fn parse_prefix(bytes: &[u8]) -> Result<(Value, usize), Error> {
    let mut reader = Reader { bytes, at: 0, items: 0 };
    let value = reader.value(0)?;
    Ok((value, reader.at))
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    items: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], Error> {
        let end = self.at.checked_add(n).ok_or(Error::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(Error::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    /// The argument that follows an initial byte: its own five bits, or the 1, 2, 4 or 8 bytes after it
    fn argument(&mut self, info: u8) -> Result<u64, Error> {
        match info {
            0..=23 => Ok(u64::from(info)),
            24 => Ok(u64::from(self.byte()?)),
            25 => Ok(u64::from(u16::from_be_bytes(self.take(2)?.try_into().map_err(|_| Error::Truncated)?))),
            26 => Ok(u64::from(u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| Error::Truncated)?))),
            27 => Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(|_| Error::Truncated)?)),
            // 28 to 30 are not used, 31 is an indefinite length
            _ => Err(Error::Unsupported),
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        self.items += 1;
        if self.items > MAX_ITEMS {
            return Err(Error::TooBig);
        }
        let initial = self.byte()?;
        let (major, info) = (initial >> 5, initial & 0x1f);
        match major {
            0 => Ok(Value::Int(i64::try_from(self.argument(info)?).map_err(|_| Error::Unsupported)?)),
            // -1 - n: the largest n that still fits is i64::MAX
            1 => Ok(Value::Int(-1 - i64::try_from(self.argument(info)?).map_err(|_| Error::Unsupported)?)),
            2 | 3 => {
                let len = usize::try_from(self.argument(info)?).map_err(|_| Error::TooBig)?;
                if len > MAX_STRING {
                    return Err(Error::TooBig);
                }
                let raw = self.take(len)?.to_vec();
                if major == 2 { Ok(Value::Bytes(raw)) } else { String::from_utf8(raw).map(Value::Text).map_err(|_| Error::Text) }
            }
            4 => {
                let len = self.length(info, depth)?;
                (0..len).map(|_| self.value(depth + 1)).collect::<Result<Vec<_>, _>>().map(Value::Array)
            }
            5 => {
                let len = self.length(info, depth)?;
                (0..len).map(|_| Ok((self.value(depth + 1)?, self.value(depth + 1)?))).collect::<Result<Vec<_>, _>>().map(Value::Map)
            }
            7 => match info {
                20 => Ok(Value::Bool(false)),
                21 => Ok(Value::Bool(true)),
                22 => Ok(Value::Null),
                _ => Err(Error::Unsupported),
            },
            // 6: a tag
            _ => Err(Error::Unsupported),
        }
    }

    /// The number of elements of an array or pairs of a map, when it may still be read
    fn length(&mut self, info: u8, depth: usize) -> Result<usize, Error> {
        if depth >= MAX_DEPTH {
            return Err(Error::TooBig);
        }
        let len = usize::try_from(self.argument(info)?).map_err(|_| Error::TooBig)?;
        // Each element takes at least a byte: a length the rest of the message can't hold is a lie, refused before it
        // reserves anything
        if len > self.bytes.len().saturating_sub(self.at) {
            return Err(Error::Truncated);
        }
        Ok(len)
    }
}

#[cfg(test)]
mod tests;
