//! ZIP files. A streaming writer: stored (downloads, whose length is known up front) or compressed with deflate
//! ("Compress to ZIP"); it writes while reading, with no temporary file, and uses ZIP64 when sizes exceed 4 GB or there
//! are more than 65535 entries. And a small reader for "Extract all", for stored and deflated entries.

use std::io::{Read, Seek, SeekFrom};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const U32_MAX: u64 = 0xFFFF_FFFF;
const STORE: u16 = 0;
const DEFLATE: u16 = 8;

struct Entry {
    name: Vec<u8>,
    crc: u32,
    size: u64,
    /// Size as written (compressed)
    csize: u64,
    method: u16,
    offset: u64,
    is_dir: bool,
    zip64: bool,
    time: u16,
    date: u16,
}

pub struct ZipWriter<W> {
    w: W,
    offset: u64,
    entries: Vec<Entry>,
    deflate: bool,
    /// Tests: names are written as given (to make archives with harmful names)
    raw_names: bool,
}

/// Unix seconds → DOS date/time (the caller shifts the seconds to the local time the ZIP should show)
fn dos_datetime(ts: i64) -> (u16, u16) {
    let days = ts.div_euclid(86400);
    let secs = ts.rem_euclid(86400);
    let (y, m, d) = crate::util::civil_from_days(days);
    if y < 1980 {
        return (0, (1 << 5) | 1);
    }
    let time = ((secs / 3600) << 11 | (secs % 3600 / 60) << 5 | ((secs % 60) / 2)) as u16;
    let date = (((y - 1980) << 9) | (m << 5) | d) as u16;
    (time, date)
}

impl<W: AsyncWrite + Unpin> ZipWriter<W> {
    /// Files are stored as they are (the length of the ZIP can be computed in advance, see `Length`)
    pub fn new(w: W) -> Self {
        Self { w, offset: 0, entries: Vec::new(), deflate: false, raw_names: false }
    }

    /// Tests: writes names as they are given, without `entry_name`
    #[cfg(test)]
    pub fn raw_names(self) -> Self {
        Self { raw_names: true, ..self }
    }

    fn name(&self, path: &str) -> String {
        if self.raw_names { path.to_string() } else { entry_name(path) }
    }

    /// Files are compressed with deflate
    pub fn deflating(w: W) -> Self {
        Self { deflate: true, ..Self::new(w) }
    }

    async fn put(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.w.write_all(buf).await?;
        self.offset += buf.len() as u64;
        Ok(())
    }

    async fn local_header(&mut self, name: &[u8], zip64: bool, is_dir: bool, method: u16, time: u16, date: u16) -> std::io::Result<()> {
        if name.len() > u16::MAX as usize {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "ZIP entry name is too long"));
        }
        let mut h = Vec::with_capacity(30 + name.len() + 20);
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&(if zip64 { 45u16 } else { 20u16 }).to_le_bytes());
        // bit 3: size and CRC are written after the data; bit 11: file name is UTF-8
        let flags: u16 = if is_dir { 0x0800 } else { 0x0808 };
        h.extend_from_slice(&flags.to_le_bytes());
        h.extend_from_slice(&method.to_le_bytes());
        h.extend_from_slice(&time.to_le_bytes());
        h.extend_from_slice(&date.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes()); // crc
        let sz: u32 = if zip64 { u32::MAX } else { 0 };
        h.extend_from_slice(&sz.to_le_bytes());
        h.extend_from_slice(&sz.to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&(if zip64 { 20u16 } else { 0u16 }).to_le_bytes());
        h.extend_from_slice(name);
        if zip64 {
            h.extend_from_slice(&1u16.to_le_bytes());
            h.extend_from_slice(&16u16.to_le_bytes());
            h.extend_from_slice(&0u64.to_le_bytes());
            h.extend_from_slice(&0u64.to_le_bytes());
        }
        self.put(&h).await
    }

    pub async fn add_dir(&mut self, path: &str, mtime: i64) -> std::io::Result<()> {
        let name = format!("{}/", self.name(path.trim_end_matches('/'))).into_bytes();
        let (time, date) = dos_datetime(mtime);
        let offset = self.offset;
        self.local_header(&name, false, true, STORE, time, date).await?;
        self.entries.push(Entry { name, crc: 0, size: 0, csize: 0, method: STORE, offset, is_dir: true, zip64: offset >= U32_MAX, time, date });
        Ok(())
    }

    /// Writes one file; `size` is the expected size, and an error is returned if the number of bytes actually read differs.
    pub async fn add_file<R: AsyncRead + Unpin>(&mut self, path: &str, mut r: R, size: u64, mtime: i64) -> std::io::Result<()> {
        let name = self.name(path).into_bytes();
        let (time, date) = dos_datetime(mtime);
        let offset = self.offset;
        // Deflate can make incompressible data a little larger: leave room for that
        let zip64 = size.saturating_add(if self.deflate { size / 1000 + 1024 } else { 0 }) >= U32_MAX || offset >= U32_MAX;
        let method = if self.deflate { DEFLATE } else { STORE };
        self.local_header(&name, zip64, false, method, time, date).await?;

        let mut hasher = crc32fast::Hasher::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut written = 0u64;
        let start = self.offset;
        // Compression runs on a blocking thread, one piece at a time
        let mut encoder = self.deflate.then(|| flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default()));
        loop {
            let n = r.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            written += n as u64;
            match encoder.take() {
                None => self.put(&buf[..n]).await?,
                Some(mut enc) => {
                    let piece = buf[..n].to_vec();
                    let (enc, out) = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
                        std::io::Write::write_all(&mut enc, &piece)?;
                        let out = std::mem::take(enc.get_mut());
                        Ok((enc, out))
                    })
                    .await
                    .map_err(std::io::Error::other)??;
                    self.put(&out).await?;
                    encoder = Some(enc);
                }
            }
        }
        if let Some(enc) = encoder {
            let out = tokio::task::spawn_blocking(move || enc.finish()).await.map_err(std::io::Error::other)??;
            self.put(&out).await?;
        }
        if written != size {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, format!("{path}: file size mismatch")));
        }
        let csize = self.offset - start;
        if !zip64 && csize >= U32_MAX {
            return Err(std::io::Error::other(format!("{path}: compressed size out of range")));
        }
        let crc = hasher.finalize();

        let mut d = Vec::with_capacity(24);
        d.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
        d.extend_from_slice(&crc.to_le_bytes());
        if zip64 {
            d.extend_from_slice(&csize.to_le_bytes());
            d.extend_from_slice(&size.to_le_bytes());
        } else {
            d.extend_from_slice(&(csize as u32).to_le_bytes());
            d.extend_from_slice(&(size as u32).to_le_bytes());
        }
        self.put(&d).await?;
        self.entries.push(Entry { name, crc, size, csize, method, offset, is_dir: false, zip64, time, date });
        Ok(())
    }

    pub async fn finish(mut self) -> std::io::Result<W> {
        let cd_start = self.offset;
        let entries = std::mem::take(&mut self.entries);
        for e in &entries {
            let need_size64 = e.size >= U32_MAX || e.csize >= U32_MAX || (e.zip64 && !e.is_dir);
            let need_off64 = e.offset >= U32_MAX;
            let mut extra = Vec::new();
            if need_size64 {
                // ZIP64 extended information: the original size comes first, then the compressed one
                extra.extend_from_slice(&e.size.to_le_bytes());
                extra.extend_from_slice(&e.csize.to_le_bytes());
            }
            if need_off64 {
                extra.extend_from_slice(&e.offset.to_le_bytes());
            }
            let mut h = Vec::with_capacity(46 + e.name.len() + 32);
            h.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            h.extend_from_slice(&45u16.to_le_bytes()); // made by
            h.extend_from_slice(&(if need_size64 || need_off64 { 45u16 } else { 20u16 }).to_le_bytes());
            h.extend_from_slice(&(if e.is_dir { 0x0800u16 } else { 0x0808u16 }).to_le_bytes());
            h.extend_from_slice(&e.method.to_le_bytes());
            h.extend_from_slice(&e.time.to_le_bytes());
            h.extend_from_slice(&e.date.to_le_bytes());
            h.extend_from_slice(&e.crc.to_le_bytes());
            let (csz, sz) = if need_size64 { (u32::MAX, u32::MAX) } else { (e.csize as u32, e.size as u32) };
            h.extend_from_slice(&csz.to_le_bytes());
            h.extend_from_slice(&sz.to_le_bytes());
            h.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            h.extend_from_slice(&(if extra.is_empty() { 0u16 } else { extra.len() as u16 + 4 }).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes()); // comment
            h.extend_from_slice(&0u16.to_le_bytes()); // disk
            h.extend_from_slice(&0u16.to_le_bytes()); // internal attr
            h.extend_from_slice(&(if e.is_dir { 0x10u32 } else { 0u32 }).to_le_bytes());
            h.extend_from_slice(&(if need_off64 { u32::MAX } else { e.offset as u32 }).to_le_bytes());
            h.extend_from_slice(&e.name);
            if !extra.is_empty() {
                h.extend_from_slice(&1u16.to_le_bytes());
                h.extend_from_slice(&(extra.len() as u16).to_le_bytes());
                h.extend_from_slice(&extra);
            }
            self.put(&h).await?;
        }
        let cd_size = self.offset - cd_start;
        let count = entries.len() as u64;

        if count >= 0xFFFF || cd_start >= U32_MAX || cd_size >= U32_MAX {
            let z64_start = self.offset;
            let mut r = Vec::with_capacity(76);
            r.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
            r.extend_from_slice(&44u64.to_le_bytes());
            r.extend_from_slice(&45u16.to_le_bytes());
            r.extend_from_slice(&45u16.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&count.to_le_bytes());
            r.extend_from_slice(&count.to_le_bytes());
            r.extend_from_slice(&cd_size.to_le_bytes());
            r.extend_from_slice(&cd_start.to_le_bytes());
            r.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
            r.extend_from_slice(&0u32.to_le_bytes());
            r.extend_from_slice(&z64_start.to_le_bytes());
            r.extend_from_slice(&1u32.to_le_bytes());
            self.put(&r).await?;
        }

        let mut e = Vec::with_capacity(22);
        e.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        let c16 = count.min(0xFFFF) as u16;
        e.extend_from_slice(&c16.to_le_bytes());
        e.extend_from_slice(&c16.to_le_bytes());
        e.extend_from_slice(&(cd_size.min(U32_MAX) as u32).to_le_bytes());
        e.extend_from_slice(&(cd_start.min(U32_MAX) as u32).to_le_bytes());
        e.extend_from_slice(&0u16.to_le_bytes());
        self.put(&e).await?;
        self.w.flush().await?;
        Ok(self.w)
    }
}

/// Computes the total ZIP size before streaming (matching ZipWriter's output byte for byte),
/// so the response can carry Content-Length and the browser can show download progress and time remaining.
/// Each item is (path, size, is folder)
/// A path as a ZIP entry name that every extractor keeps inside the folder it extracts to: in each part (between the
/// '/'), characters Windows treats as separators or can't store (`\ : * ? " < > |`, ASCII control characters) become '_', and
/// "." or ".." becomes "_" or "__". Names from a folder on the server may hold any of them (Linux allows all but '/').
/// The length in bytes stays the same, so `Length` still holds.
fn entry_name(path: &str) -> String {
    path.split('/')
        .map(|part| match part {
            "." | ".." => "_".repeat(part.len()),
            _ => part.chars().map(|c| if c.is_ascii_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect(),
        })
        .collect::<Vec<String>>()
        .join("/")
}

#[cfg(test)]
pub fn predicted_len<'a>(items: impl IntoIterator<Item = (&'a str, u64, bool)>) -> u64 {
    let mut len = Length::default();
    for (path, size, is_dir) in items {
        len.add(path, size, is_dir);
    }
    len.total()
}

/// The length of a stored ZIP, added up an entry at a time (in the order they are written)
#[derive(Default)]
pub struct Length {
    offset: u64,
    central: u64,
    count: u64,
}

impl Length {
    pub fn add(&mut self, path: &str, size: u64, is_dir: bool) {
        self.count += 1;
        let entry_offset = self.offset;
        let (name_len, size64) = if is_dir {
            let name_len = path.trim_end_matches('/').len() as u64 + 1;
            self.offset += 30 + name_len;
            (name_len, false)
        } else {
            let name_len = path.len() as u64;
            let zip64 = size >= U32_MAX || self.offset >= U32_MAX;
            // Local header (20 more for ZIP64) + content + data descriptor (24 for ZIP64, otherwise 16)
            self.offset += 30 + name_len + if zip64 { 20 } else { 0 } + size + if zip64 { 24 } else { 16 };
            (name_len, zip64)
        };
        let extra = if size64 { 16 } else { 0 } + if entry_offset >= U32_MAX { 8 } else { 0 };
        self.central += 46 + name_len + if extra > 0 { extra + 4 } else { 0 };
    }

    pub fn total(&self) -> u64 {
        let cd_start = self.offset;
        let zip64_end = self.count >= 0xFFFF || cd_start >= U32_MAX || self.central >= U32_MAX;
        cd_start + self.central + if zip64_end { 76 } else { 0 } + 22
    }
}

// ───────────── Reading ─────────────

/// An entry of a ZIP file, from its central directory
#[derive(Debug, Clone)]
pub struct ReadEntry {
    /// The path inside the archive, as written there ("/" between folders; a folder ends with "/")
    pub name: String,
    pub is_dir: bool,
    pub method: u16,
    pub encrypted: bool,
    pub crc: u32,
    /// Size in the archive
    pub csize: u64,
    /// Size once extracted, as the archive states it
    pub size: u64,
    header_offset: u64,
}

impl ReadEntry {
    /// Whether this reader can extract it: stored or deflated, not encrypted
    pub fn supported(&self) -> bool {
        !self.encrypted && matches!(self.method, STORE | DEFLATE)
    }
}

fn bad(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, msg.to_string())
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

/// Code page 437, which ZIP names use when they aren't marked as UTF-8 (bytes 128–255)
const CP437: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

fn decode_name(raw: &[u8], utf8: bool) -> String {
    if utf8 || raw.is_ascii() {
        return String::from_utf8_lossy(raw).into_owned();
    }
    // Some tools write UTF-8 without saying so
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }
    let high: Vec<char> = CP437.chars().collect();
    raw.iter().map(|&b| if b < 128 { b as char } else { high[b as usize - 128] }).collect()
}

/// Reads the list of entries. More than `max_entries` is refused before anything is allocated for them, and so is a
/// central directory larger than `max_directory` bytes
pub fn read_entries<R: Read + Seek>(r: &mut R, max_entries: u64, max_directory: u64) -> std::io::Result<Vec<ReadEntry>> {
    let len = r.seek(SeekFrom::End(0))?;
    // The end record is the last 22 bytes, unless a comment (up to 65535 bytes) follows it
    let tail_len = len.min(22 + 0xFFFF);
    r.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = vec![0u8; tail_len as usize];
    r.read_exact(&mut tail)?;
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| u32_at(&tail, i) == 0x0605_4b50 && i + 22 + u16_at(&tail, i + 20) as usize == tail.len())
        .ok_or_else(|| bad("This isn't a ZIP file, or it is damaged"))?;
    let e = &tail[eocd..];
    if u16_at(e, 4) != 0 || u16_at(e, 6) != 0 {
        return Err(bad("ZIP files split into several parts aren't supported"));
    }
    let (mut count, mut cd_size, mut cd_offset) = (u16_at(e, 10) as u64, u32_at(e, 12) as u64, u32_at(e, 16) as u64);
    if count == 0xFFFF || cd_size == U32_MAX || cd_offset == U32_MAX {
        // ZIP64: a locator just before the end record points to the ZIP64 end record
        let at = (len - tail_len) + eocd as u64;
        if at < 20 {
            return Err(bad("This isn't a ZIP file, or it is damaged"));
        }
        let mut loc = [0u8; 20];
        r.seek(SeekFrom::Start(at - 20))?;
        r.read_exact(&mut loc)?;
        if u32_at(&loc, 0) != 0x0706_4b50 {
            return Err(bad("This isn't a ZIP file, or it is damaged"));
        }
        let mut rec = [0u8; 56];
        r.seek(SeekFrom::Start(u64_at(&loc, 8)))?;
        r.read_exact(&mut rec)?;
        if u32_at(&rec, 0) != 0x0606_4b50 {
            return Err(bad("This isn't a ZIP file, or it is damaged"));
        }
        (count, cd_size, cd_offset) = (u64_at(&rec, 32), u64_at(&rec, 40), u64_at(&rec, 48));
    }
    if count > max_entries {
        return Err(std::io::Error::new(std::io::ErrorKind::FileTooLarge, "too many entries"));
    }
    if cd_size > max_directory || cd_offset.checked_add(cd_size).is_none_or(|end| end > len) {
        return Err(bad("This isn't a ZIP file, or it is damaged"));
    }
    let mut cd = vec![0u8; cd_size as usize];
    r.seek(SeekFrom::Start(cd_offset))?;
    r.read_exact(&mut cd)?;
    let mut out = Vec::with_capacity(count as usize);
    let mut i = 0usize;
    for _ in 0..count {
        if i + 46 > cd.len() || u32_at(&cd, i) != 0x0201_4b50 {
            return Err(bad("This isn't a ZIP file, or it is damaged"));
        }
        let h = &cd[i..];
        let flags = u16_at(h, 8);
        let (nlen, elen, clen) = (u16_at(h, 28) as usize, u16_at(h, 30) as usize, u16_at(h, 32) as usize);
        if i + 46 + nlen + elen + clen > cd.len() {
            return Err(bad("This isn't a ZIP file, or it is damaged"));
        }
        let raw_name = &h[46..46 + nlen];
        let extra = &h[46 + nlen..46 + nlen + elen];
        let (mut csize, mut size, mut header_offset) = (u32_at(h, 20) as u64, u32_at(h, 24) as u64, u32_at(h, 42) as u64);
        let mut name = decode_name(raw_name, flags & 0x0800 != 0);
        let mut j = 0;
        while j + 4 <= extra.len() {
            let (id, n) = (u16_at(extra, j), u16_at(extra, j + 2) as usize);
            let data = extra.get(j + 4..j + 4 + n).ok_or_else(|| bad("This isn't a ZIP file, or it is damaged"))?;
            match id {
                // ZIP64: the fields that didn't fit, in this order
                0x0001 => {
                    let mut k = 0;
                    for field in [&mut size, &mut csize, &mut header_offset] {
                        if *field == U32_MAX {
                            *field = data.get(k..k + 8).map(|b| u64_at(b, 0)).ok_or_else(|| bad("This isn't a ZIP file, or it is damaged"))?;
                            k += 8;
                        }
                    }
                }
                // Unicode path: the name in UTF-8, when it belongs to this name (checked by its CRC)
                0x7075 if n > 5 && data[0] == 1 && u32_at(data, 1) == crc32fast::hash(raw_name) => {
                    name = String::from_utf8_lossy(&data[5..]).into_owned();
                }
                _ => {}
            }
            j += 4 + n;
        }
        out.push(ReadEntry {
            is_dir: name.ends_with('/') || name.ends_with('\\'),
            name,
            method: u16_at(h, 10),
            encrypted: flags & 1 != 0,
            crc: u32_at(h, 16),
            csize,
            size,
            header_offset,
        });
        i += 46 + nlen + elen + clen;
    }
    Ok(out)
}

/// The extracted content of an entry (stored or deflated; see `ReadEntry::supported`). Reads at most what the entry
/// states as its compressed size; the caller limits and checks what comes out
pub fn open_entry<'a, R: Read + Seek>(r: &'a mut R, e: &ReadEntry) -> std::io::Result<Box<dyn Read + 'a>> {
    if !e.supported() {
        return Err(bad("unsupported entry"));
    }
    let mut h = [0u8; 30];
    r.seek(SeekFrom::Start(e.header_offset))?;
    r.read_exact(&mut h)?;
    if u32_at(&h, 0) != 0x0403_4b50 {
        return Err(bad("This isn't a ZIP file, or it is damaged"));
    }
    let skip = u16_at(&h, 26) as i64 + u16_at(&h, 28) as i64;
    r.seek(SeekFrom::Current(skip))?;
    let data = r.take(e.csize);
    Ok(match e.method {
        DEFLATE => Box::new(flate2::read::DeflateDecoder::new(data)),
        _ => Box::new(data),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn actual_len(items: &[(&str, u64, bool)]) -> u64 {
        let mut zip = ZipWriter::new(Vec::new());
        for (path, size, is_dir) in items {
            if *is_dir {
                zip.add_dir(path, 1_700_000_000).await.unwrap();
            } else {
                zip.add_file(path, &vec![7u8; *size as usize][..], *size, 1_700_000_000).await.unwrap();
            }
        }
        zip.finish().await.unwrap().len() as u64
    }

    #[tokio::test]
    async fn predicted_length_matches_the_output() {
        // CJK and emoji names check that multi-byte UTF-8 name lengths are counted correctly
        let items = [("報告/", 0, true), ("報告/年度.docx", 12_345, false), ("報告/空白.txt", 0, false), ("照片", 0, true), ("照片/😀 很長的檔名.jpg", 70_000, false)];
        assert_eq!(predicted_len(items), actual_len(&items).await);
        assert_eq!(predicted_len([]), actual_len(&[]).await);
        // More than 65535 entries switches to the ZIP64 end record
        let many: Vec<String> = (0..70_000).map(|i| format!("d{i}")).collect();
        let items: Vec<(&str, u64, bool)> = many.iter().map(|n| (n.as_str(), 0, true)).collect();
        assert_eq!(predicted_len(items.iter().copied()), actual_len(&items).await);
    }

    /// Writes a ZIP (deflated or stored) and reads it back with the reader
    async fn round_trip(deflate: bool) {
        let big = "Lorem ipsum dolor sit amet. ".repeat(20_000);
        let files: [(&str, &[u8]); 3] = [("報告/年度.txt", big.as_bytes()), ("報告/空白.txt", b""), ("a.bin", &[1, 2, 3, 250, 0, 7])];
        let mut zip = if deflate { ZipWriter::deflating(Vec::new()) } else { ZipWriter::new(Vec::new()) };
        zip.add_dir("報告", 1_700_000_000).await.unwrap();
        for (path, data) in files {
            zip.add_file(path, data, data.len() as u64, 1_700_000_000).await.unwrap();
        }
        let bytes = zip.finish().await.unwrap();
        if deflate {
            assert!(bytes.len() < big.len() / 10, "compressed to {} bytes", bytes.len());
        }
        let mut cursor = std::io::Cursor::new(bytes);
        let entries = read_entries(&mut cursor, 100, 1 << 20).unwrap();
        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["報告/", "報告/年度.txt", "報告/空白.txt", "a.bin"]);
        assert!(entries[0].is_dir && !entries[1].is_dir);
        for (entry, (_, data)) in entries[1..].iter().zip(files) {
            assert_eq!(entry.size, data.len() as u64);
            let mut out = Vec::new();
            open_entry(&mut cursor, entry).unwrap().read_to_end(&mut out).unwrap();
            assert_eq!(out, data);
            assert_eq!(crc32fast::hash(&out), entry.crc);
        }
        // Too many entries is refused up front
        assert_eq!(read_entries(&mut cursor, 3, 1 << 20).unwrap_err().kind(), std::io::ErrorKind::FileTooLarge);
    }

    #[tokio::test]
    async fn names_windows_would_read_as_paths_are_made_harmless() {
        // A name Linux allows (a folder on the server can hold it): Windows tools would split it at the backslashes
        let evil = r"..\..\AppData\Roaming\Microsoft\Windows\Start Menu\Programs\Startup\x.bat";
        let mut zip = ZipWriter::new(Vec::new());
        zip.add_dir("Docs/..", 1_700_000_000).await.unwrap();
        zip.add_file(&format!("Docs/{evil}"), &b"x"[..], 1, 1_700_000_000).await.unwrap();
        zip.add_file("Docs/a:b?.txt", &b"y"[..], 1, 1_700_000_000).await.unwrap();
        let predicted = predicted_len([("Docs/..", 0, true), (format!("Docs/{evil}").as_str(), 1, false), ("Docs/a:b?.txt", 1, false)]);
        let bytes = zip.finish().await.unwrap();
        assert_eq!(bytes.len() as u64, predicted, "the length announced up front still holds");
        let entries = read_entries(&mut std::io::Cursor::new(bytes), 10, 1 << 20).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Docs/__/", r"Docs/.._.._AppData_Roaming_Microsoft_Windows_Start Menu_Programs_Startup_x.bat", "Docs/a_b_.txt"]);
        assert!(names.iter().all(|n| !n.contains('\\') && !n.split('/').any(|p| p == "..")));
    }

    #[tokio::test]
    async fn deflated_and_stored_zips_read_back() {
        round_trip(true).await;
        round_trip(false).await;
    }

    #[test]
    fn names_not_marked_as_utf8_are_code_page_437() {
        assert_eq!(decode_name(b"caf\x82.txt", false), "café.txt");
        assert_eq!(decode_name("日本.txt".as_bytes(), false), "日本.txt");
        let mut cursor = std::io::Cursor::new(b"not a zip at all".to_vec());
        assert!(read_entries(&mut cursor, 10, 1 << 20).is_err());
    }

    #[test]
    fn dos_time() {
        // 2024-02-29 13:45:30 UTC
        let (t, d) = dos_datetime(1_709_214_330);
        assert_eq!(d, ((2024 - 1980) << 9 | 2 << 5 | 29) as u16);
        assert_eq!(t, (13 << 11 | 45 << 5 | 15) as u16);
    }
}
