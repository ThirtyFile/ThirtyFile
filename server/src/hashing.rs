//! Reading and copying content while computing its SHA-256 (the content store names content by it), and the errors
//! that say content read or copied can't be used.

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Reads `src` to its end, writing what it reads to `dst`: returns its SHA-256 (hex) and length
pub fn copy(src: &mut impl Read, dst: &mut impl Write) -> io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut len = 0u64;
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n])?;
        len += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), len))
}

/// The SHA-256 (hex) and length of a file on this server, read on a blocking thread
pub async fn file(path: PathBuf) -> io::Result<(String, u64)> {
    tokio::task::spawn_blocking(move || copy(&mut std::fs::File::open(path)?, &mut io::sink())).await.map_err(io::Error::other)?
}

/// Reads `src` to its end, writing what it reads to `dst`: returns its SHA-256 (hex) and length
pub async fn copy_async<R, W>(src: &mut R, dst: &mut W) -> io::Result<(String, u64)>
where
    R: AsyncRead + Unpin + ?Sized,
    W: AsyncWrite + Unpin + ?Sized,
{
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut len = 0u64;
    loop {
        let n = src.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n]).await?;
        len += n as u64;
    }
    Ok((hex::encode(hasher.finalize()), len))
}

/// The SHA-256 (hex) and length of what `src` reads
pub async fn read_async<R: AsyncRead + Unpin + ?Sized>(src: &mut R) -> io::Result<(String, u64)> {
    copy_async(src, &mut tokio::io::sink()).await
}

/// Copies `src` into a new file `tmp`, on the disk before this returns, and checks that it is `size` bytes with the
/// SHA-256 `hash`; when it isn't, the error is `Unusable::Damaged` with `message` as its text
pub async fn copy_checked<R: AsyncRead + Unpin + ?Sized>(src: &mut R, hash: &str, size: u64, tmp: &Path, message: &'static str) -> io::Result<()> {
    let mut file = tokio::fs::File::create(tmp).await?;
    let (got, len) = copy_async(src, &mut file).await?;
    file.flush().await?;
    file.sync_all().await?;
    if got != hash || len != size {
        return Err(unusable(Unusable::Damaged, message));
    }
    Ok(())
}

/// Why content read or copied can't be used. Recognised by what it is (`unusable_kind`), not by its text, which is what
/// people are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unusable {
    /// A file changed while it was read
    Changed,
    /// Content didn't match its SHA-256 or its size
    Damaged,
}

#[derive(Debug)]
struct UnusableError {
    kind: Unusable,
    message: &'static str,
}

impl std::fmt::Display for UnusableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for UnusableError {}

/// An error saying the content can't be used, with `message` as its text
pub fn unusable(kind: Unusable, message: &'static str) -> io::Error {
    io::Error::other(UnusableError { kind, message })
}

/// What made content unusable, when that is what `e` says
pub fn unusable_kind(e: &io::Error) -> Option<Unusable> {
    e.get_ref().and_then(|inner| inner.downcast_ref::<UnusableError>()).map(|u| u.kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn copies_are_hashed_and_checked() {
        let body = b"the content".to_vec();
        let expected = (crate::util::sha256_hex(&body), body.len() as u64);
        let mut out = Vec::new();
        assert_eq!(copy(&mut body.as_slice(), &mut out).unwrap(), expected);
        assert_eq!(out, body);
        assert_eq!(read_async(&mut body.as_slice()).await.unwrap(), expected);

        let dir = std::env::temp_dir().join(format!("thirtyfile-hashing-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tmp = dir.join("copy");
        copy_checked(&mut body.as_slice(), &expected.0, expected.1, &tmp, "damaged").await.unwrap();
        assert_eq!(file(tmp.clone()).await.unwrap(), expected);
        let e = copy_checked(&mut &b"other content"[..], &expected.0, expected.1, &dir.join("other"), "damaged").await.unwrap_err();
        assert_eq!((unusable_kind(&e), e.to_string().as_str()), (Some(Unusable::Damaged), "damaged"));
        assert_eq!(unusable_kind(&io::Error::other("damaged")), None, "recognised by what it is, not by its text");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
