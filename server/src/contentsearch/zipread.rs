//! Reading the files of a ZIP held in memory: what an Office document is. Only what those need: the central directory,
//! stored and deflated entries, no encryption and no ZIP64. What an entry expands to is capped by the caller, so a
//! small file that expands to gigabytes (a "zip bomb") costs a bounded amount of memory.

use std::io::Read;

const END_SIG: u32 = 0x0605_4b50;
const CENTRAL_SIG: u32 = 0x0201_4b50;
const LOCAL_SIG: u32 = 0x0403_4b50;
/// The end record is the last 22 bytes plus a comment of at most 65,535
const END_SEARCH: usize = 22 + 65_535;
/// Entries read from one archive at most (a document has a few dozen; a bomb of entries is refused)
const MAX_ENTRIES: usize = 20_000;

pub struct Entry {
    pub name: String,
    method: u16,
    compressed: u64,
    offset: u64,
}

pub struct Archive<'a> {
    data: &'a [u8],
    pub entries: Vec<Entry>,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

impl<'a> Archive<'a> {
    /// The archive's entries, or None when it isn't a ZIP this can read
    pub fn parse(data: &'a [u8]) -> Option<Archive<'a>> {
        let from = data.len().saturating_sub(END_SEARCH);
        let end = (from..data.len().saturating_sub(21)).rev().find(|&i| u32_at(data, i) == Some(END_SIG))?;
        let count = usize::from(u16_at(data, end + 10)?);
        let dir = u32_at(data, end + 16)?;
        // ZIP64 (or an archive of more than 65,534 entries): not read
        if count == 0xFFFF || dir == 0xFFFF_FFFF || count > MAX_ENTRIES {
            return None;
        }
        let mut at = usize::try_from(dir).ok()?;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            if u32_at(data, at)? != CENTRAL_SIG {
                return None;
            }
            let flags = u16_at(data, at + 8)?;
            let (method, compressed) = (u16_at(data, at + 10)?, u64::from(u32_at(data, at + 20)?));
            let (name_len, extra_len, comment_len) = (usize::from(u16_at(data, at + 28)?), usize::from(u16_at(data, at + 30)?), usize::from(u16_at(data, at + 32)?));
            let offset = u64::from(u32_at(data, at + 42)?);
            let name = String::from_utf8_lossy(data.get(at + 46..at + 46 + name_len)?).into_owned();
            // An encrypted entry can't be read; it is left out
            if flags & 1 == 0 {
                entries.push(Entry { name, method, compressed, offset });
            }
            at += 46 + name_len + extra_len + comment_len;
        }
        Some(Archive { data, entries })
    }

    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// An entry's content, at most `cap` bytes of it (the rest is left unread)
    pub fn read(&self, e: &Entry, cap: usize) -> Option<Vec<u8>> {
        let at = usize::try_from(e.offset).ok()?;
        if u32_at(self.data, at)? != LOCAL_SIG {
            return None;
        }
        let start = at + 30 + usize::from(u16_at(self.data, at + 26)?) + usize::from(u16_at(self.data, at + 28)?);
        let stored = self.data.get(start..start.checked_add(usize::try_from(e.compressed).ok()?)?)?;
        match e.method {
            0 => Some(stored[..stored.len().min(cap)].to_vec()),
            8 => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(stored).take(cap as u64).read_to_end(&mut out).ok()?;
                Some(out)
            }
            _ => None,
        }
    }
}
