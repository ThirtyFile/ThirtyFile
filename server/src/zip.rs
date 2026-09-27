//! Streaming ZIP writer (STORE only, no compression).
//! Writes while reading, with no temporary file; automatically uses ZIP64 when sizes exceed 4GB or there are more than 65535 entries.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const U32_MAX: u64 = 0xFFFF_FFFF;

struct Entry {
    name: Vec<u8>,
    crc: u32,
    size: u64,
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
}

/// Unix seconds → DOS date/time (UTC)
fn dos_datetime(ts: i64) -> (u16, u16) {
    let days = ts.div_euclid(86400);
    let secs = ts.rem_euclid(86400);
    // civil_from_days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    if y < 1980 {
        return (0, (1 << 5) | 1);
    }
    let time = ((secs / 3600) << 11 | (secs % 3600 / 60) << 5 | ((secs % 60) / 2)) as u16;
    let date = (((y - 1980) << 9) | (m << 5) | d) as u16;
    (time, date)
}

impl<W: AsyncWrite + Unpin> ZipWriter<W> {
    pub fn new(w: W) -> Self {
        Self { w, offset: 0, entries: Vec::new() }
    }

    async fn put(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.w.write_all(buf).await?;
        self.offset += buf.len() as u64;
        Ok(())
    }

    async fn local_header(&mut self, name: &[u8], zip64: bool, is_dir: bool, time: u16, date: u16) -> std::io::Result<()> {
        if name.len() > u16::MAX as usize {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "ZIP entry name is too long"));
        }
        let mut h = Vec::with_capacity(30 + name.len() + 20);
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&(if zip64 { 45u16 } else { 20u16 }).to_le_bytes());
        // bit 3: size and CRC are written after the data; bit 11: file name is UTF-8
        let flags: u16 = if is_dir { 0x0800 } else { 0x0808 };
        h.extend_from_slice(&flags.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // STORE
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
        let name = format!("{}/", path.trim_end_matches('/')).into_bytes();
        let (time, date) = dos_datetime(mtime);
        let offset = self.offset;
        self.local_header(&name, false, true, time, date).await?;
        self.entries.push(Entry { name, crc: 0, size: 0, offset, is_dir: true, zip64: offset >= U32_MAX, time, date });
        Ok(())
    }

    /// Writes one file; `size` is the expected size, and an error is returned if the number of bytes actually read differs.
    pub async fn add_file<R: AsyncRead + Unpin>(&mut self, path: &str, mut r: R, size: u64, mtime: i64) -> std::io::Result<()> {
        let name = path.as_bytes().to_vec();
        let (time, date) = dos_datetime(mtime);
        let offset = self.offset;
        let zip64 = size >= U32_MAX || offset >= U32_MAX;
        self.local_header(&name, zip64, false, time, date).await?;

        let mut hasher = crc32fast::Hasher::new();
        let mut buf = vec![0u8; 256 * 1024];
        let mut written = 0u64;
        loop {
            let n = r.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            self.put(&buf[..n]).await?;
            written += n as u64;
        }
        if written != size {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, format!("{path}: file size mismatch")));
        }
        let crc = hasher.finalize();

        let mut d = Vec::with_capacity(24);
        d.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
        d.extend_from_slice(&crc.to_le_bytes());
        if zip64 {
            d.extend_from_slice(&size.to_le_bytes());
            d.extend_from_slice(&size.to_le_bytes());
        } else {
            d.extend_from_slice(&(size as u32).to_le_bytes());
            d.extend_from_slice(&(size as u32).to_le_bytes());
        }
        self.put(&d).await?;
        self.entries.push(Entry { name, crc, size, offset, is_dir: false, zip64, time, date });
        Ok(())
    }

    pub async fn finish(mut self) -> std::io::Result<W> {
        let cd_start = self.offset;
        let entries = std::mem::take(&mut self.entries);
        for e in &entries {
            let need_size64 = e.size >= U32_MAX || (e.zip64 && !e.is_dir);
            let need_off64 = e.offset >= U32_MAX;
            let mut extra = Vec::new();
            if need_size64 {
                extra.extend_from_slice(&e.size.to_le_bytes());
                extra.extend_from_slice(&e.size.to_le_bytes());
            }
            if need_off64 {
                extra.extend_from_slice(&e.offset.to_le_bytes());
            }
            let mut h = Vec::with_capacity(46 + e.name.len() + 32);
            h.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            h.extend_from_slice(&45u16.to_le_bytes()); // made by
            h.extend_from_slice(&(if need_size64 || need_off64 { 45u16 } else { 20u16 }).to_le_bytes());
            h.extend_from_slice(&(if e.is_dir { 0x0800u16 } else { 0x0808u16 }).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            h.extend_from_slice(&e.time.to_le_bytes());
            h.extend_from_slice(&e.date.to_le_bytes());
            h.extend_from_slice(&e.crc.to_le_bytes());
            let sz = if need_size64 { u32::MAX } else { e.size as u32 };
            h.extend_from_slice(&sz.to_le_bytes());
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
pub fn predicted_len<'a>(items: impl IntoIterator<Item = (&'a str, u64, bool)>) -> u64 {
    let mut offset = 0u64;
    let mut central = 0u64;
    let mut count = 0u64;
    for (path, size, is_dir) in items {
        count += 1;
        let entry_offset = offset;
        let (name_len, size64) = if is_dir {
            let name_len = path.trim_end_matches('/').len() as u64 + 1;
            offset += 30 + name_len;
            (name_len, false)
        } else {
            let name_len = path.len() as u64;
            let zip64 = size >= U32_MAX || offset >= U32_MAX;
            // Local header (20 more for ZIP64) + content + data descriptor (24 for ZIP64, otherwise 16)
            offset += 30 + name_len + if zip64 { 20 } else { 0 } + size + if zip64 { 24 } else { 16 };
            (name_len, zip64)
        };
        let extra = if size64 { 16 } else { 0 } + if entry_offset >= U32_MAX { 8 } else { 0 };
        central += 46 + name_len + if extra > 0 { extra + 4 } else { 0 };
    }
    let cd_start = offset;
    let zip64_end = count >= 0xFFFF || cd_start >= U32_MAX || central >= U32_MAX;
    cd_start + central + if zip64_end { 76 } else { 0 } + 22
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

    #[test]
    fn dos_time() {
        // 2024-02-29 13:45:30 UTC
        let (t, d) = dos_datetime(1_709_214_330);
        assert_eq!(d, ((2024 - 1980) << 9 | 2 << 5 | 29) as u16);
        assert_eq!(t, (13 << 11 | 45 << 5 | 15) as u16);
    }
}
