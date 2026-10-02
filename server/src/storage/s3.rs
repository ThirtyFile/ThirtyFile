//! S3-compatible object storage

use super::*;

pub struct S3Storage {
    store: Arc<dyn ObjectStore>,
    prefix: String,
}

#[cfg(test)]
impl S3Storage {
    /// A bucket kept in memory, reached through the same code as a real one (tests)
    pub fn in_memory(prefix: &str) -> Self {
        Self { store: Arc::new(object_store::memory::InMemory::new()), prefix: prefix.to_string() }
    }
}

/// Connect timeout: report quickly when the service can't be reached at all
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Read timeout: a connection counts as stuck only after receiving no data for this long (the total transfer time isn't limited, so large files and slow downloads aren't interrupted)
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Retry count: when the service doesn't respond, users see an error after about 45 seconds instead of waiting several minutes
const MAX_RETRIES: usize = 2;
/// Deadline for resuming from the break point when the network drops during a download (counted from the start of the download)
const RETRY_WINDOW: Duration = Duration::from_secs(30 * 60);

/// By default object_store limits a whole request (including the downloaded content) to 30 seconds with a 3-minute retry deadline:
/// downloads longer than 30 seconds keep getting interrupted and resumed, and fail after 3 minutes, so large downloads on slow networks always break.
/// Instead, only detect "stuck" connections (read timeout) without limiting the total time.
fn client_options() -> (ClientOptions, RetryConfig) {
    let options = ClientOptions::new().with_connect_timeout(CONNECT_TIMEOUT).with_read_timeout(READ_TIMEOUT).with_timeout_disabled();
    let retry = RetryConfig { backoff: BackoffConfig::default(), max_retries: MAX_RETRIES, retry_timeout: RETRY_WINDOW };
    (options, retry)
}

const MULTIPART_THRESHOLD: u64 = 16 * 1024 * 1024;
const MULTIPART_CHUNK: usize = 8 * 1024 * 1024;

/// S3 / R2 allow at most 10,000 parts: starting at 8 MiB, larger files use larger parts (in whole MiB, all parts the same size)
fn part_size(size: u64) -> usize {
    const MIB: u64 = 1024 * 1024;
    let needed = size.div_ceil(9_000).div_ceil(MIB) * MIB;
    needed.max(MULTIPART_CHUNK as u64) as usize
}

fn s3_err(e: object_store::Error) -> io::Error {
    let message = match &e {
        object_store::Error::NotFound { .. } => return io::Error::new(io::ErrorKind::NotFound, e.to_string()),
        object_store::Error::PermissionDenied { .. } | object_store::Error::Unauthenticated { .. } => DENIED_KEYS,
        _ => UNAVAILABLE,
    };
    io::Error::other(StorageError { message, detail: e.to_string() })
}

impl S3Storage {
    pub fn new(cfg: &S3Config) -> io::Result<Self> {
        let bucket = cfg.bucket.trim();
        let endpoint = cfg.endpoint.trim().trim_end_matches('/');
        // AWS's wildcard certificate (*.s3.amazonaws.com) only covers one domain level, so buckets whose names contain "." must use path-style
        let path_style = cfg.path_style || bucket.contains('.');
        let (options, retry) = client_options();
        let mut b = AmazonS3Builder::new()
            .with_client_options(options)
            .with_retry(retry)
            .with_bucket_name(bucket)
            .with_region(if cfg.region.trim().is_empty() { "us-east-1" } else { cfg.region.trim() })
            .with_access_key_id(cfg.access_key_id.trim())
            .with_secret_access_key(cfg.secret_access_key.trim())
            .with_allow_http(cfg.allow_http)
            .with_virtual_hosted_style_request(!path_style)
            // We delete one object at a time; single DELETE is a basic API every S3-compatible service supports (some lack DeleteObjects)
            .with_disable_bulk_delete(true);
        if !endpoint.is_empty() {
            // With virtual-hosted style object_store uses the endpoint as is, so we must prepend the bucket to the host name ourselves
            b = b.with_endpoint(if path_style { endpoint.to_string() } else { virtual_hosted_endpoint(endpoint, bucket)? });
        }
        let store = b.build().map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        Ok(Self { store: Arc::new(store), prefix: cfg.prefix.trim().trim_matches('/').to_string() })
    }

    fn key(&self, name: &str) -> ObjectPath {
        if self.prefix.is_empty() { ObjectPath::from(name) } else { ObjectPath::from(format!("{}/{name}", self.prefix)) }
    }

    fn blob_key(&self, hash: &str) -> io::Result<ObjectPath> {
        valid_hash(hash)?;
        Ok(self.key(&format!("blobs/{}/{}/{hash}", &hash[0..2], &hash[2..4])))
    }

    /// A path in the location as an object key (a checked path; "" is the prefix itself)
    fn key_at(&self, key: &str) -> io::Result<ObjectPath> {
        key_parts(key)?;
        Ok(ObjectPath::from(join_key(&[&self.prefix, key])))
    }

    /// Stores a temp file at `key`: one request up to 16 MiB, a multipart upload above (the temp file is kept)
    async fn put_object(&self, key: &ObjectPath, src: &Path) -> io::Result<()> {
        let size = tokio::fs::metadata(src).await?.len();
        if size <= MULTIPART_THRESHOLD {
            let data = tokio::fs::read(src).await?;
            self.store.put(key, PutPayload::from(data)).await.map_err(s3_err)?;
            return Ok(());
        }
        let upload = self.store.put_multipart(key).await.map_err(s3_err)?;
        let chunk = part_size(size);
        let mut writer = WriteMultipart::new_with_chunk_size(upload, chunk);
        // Every part (except the last) has the same size: R2 requires it and AWS accepts it
        let sent: io::Result<()> = async {
            let mut f = tokio::fs::File::open(src).await?;
            let mut buf = vec![0u8; MULTIPART_CHUNK];
            loop {
                let n = f.read(&mut buf).await?;
                if n == 0 {
                    return Ok(());
                }
                writer.wait_for_capacity(if chunk > 64 << 20 { 2 } else { 4 }).await.map_err(s3_err)?;
                writer.write(&buf[..n]);
            }
        }
        .await;
        if let Err(e) = sent {
            // Abort the multipart upload so unfinished parts don't stay in the bucket and keep incurring charges
            let _ = writer.abort().await;
            return Err(e);
        }
        writer.finish().await.map_err(s3_err)?;
        Ok(())
    }

    async fn open_object(&self, key: &ObjectPath, start: u64, len: u64) -> io::Result<BoxReader> {
        if len == 0 {
            return Ok(Box::pin(tokio::io::empty()) as BoxReader);
        }
        let opts = GetOptions::new().with_range(Some(start..start + len));
        let result = self.store.get_opts(key, opts).await.map_err(s3_err)?;
        let stream = result.into_stream().map_err(s3_err).boxed();
        Ok(Box::pin(StreamReader::new(stream)) as BoxReader)
    }

    async fn delete_object(&self, key: &ObjectPath) -> io::Result<()> {
        match self.store.delete(key).await {
            Err(object_store::Error::NotFound { .. }) | Ok(()) => Ok(()),
            Err(e) => Err(s3_err(e)),
        }
    }
}

fn object_entry(meta: &object_store::ObjectMeta) -> Entry {
    Entry::file(meta.location.filename().unwrap_or_default(), meta.size, Some(meta.last_modified.timestamp()))
}

impl Storage for S3Storage {
    fn put_file<'a>(&'a self, hash: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.put_object(&self.blob_key(hash)?, src).await?;
            let _ = tokio::fs::remove_file(src).await;
            Ok(())
        })
    }

    fn open<'a>(&'a self, hash: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(async move { self.open_object(&self.blob_key(hash)?, start, len).await })
    }

    fn delete<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move { self.delete_object(&self.blob_key(hash)?).await })
    }

    fn list_dir<'a>(&'a self, dir: &'a str) -> BoxFuture<'a, io::Result<Vec<Entry>>> {
        Box::pin(async move {
            let prefix = self.key_at(dir)?;
            let listed = self.store.list_with_delimiter(Some(&prefix)).await.map_err(s3_err)?;
            let folders = listed.common_prefixes.iter().filter_map(|p| p.filename()).map(|name| Entry {
                name: name.to_string(),
                kind: EntryKind::Folder,
                size: 0,
                modified: None,
            });
            Ok(folders.chain(listed.objects.iter().map(object_entry)).collect())
        })
    }

    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<Entry>>> {
        Box::pin(async move {
            match self.store.head(&self.key_at(key)?).await {
                Ok(meta) => Ok(Some(object_entry(&meta))),
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(e) => Err(s3_err(e)),
            }
        })
    }

    fn put_at<'a>(&'a self, key: &'a str, src: &'a Path) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            self.put_object(&self.key_at(key)?, src).await?;
            let _ = tokio::fs::remove_file(src).await;
            Ok(())
        })
    }

    fn open_at<'a>(&'a self, key: &'a str, start: u64, len: u64) -> BoxFuture<'a, io::Result<BoxReader>> {
        Box::pin(async move { self.open_object(&self.key_at(key)?, start, len).await })
    }

    fn delete_at<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<()>> {
        Box::pin(async move { self.delete_object(&self.key_at(key)?).await })
    }

    fn list_content<'a>(&'a self, seen: &'a (dyn Fn(u64) + Send + Sync)) -> BoxFuture<'a, io::Result<Vec<Entry>>> {
        Box::pin(async move {
            // One listing of everything below blobs/ (a request per 1000 objects), not one per folder level
            let base = self.key("blobs");
            let mut objects = self.store.list(Some(&base));
            let mut out = Vec::new();
            while let Some(meta) = objects.next().await {
                let meta = meta.map_err(s3_err)?;
                let rel: Vec<_> = meta.location.prefix_match(&base).map(|p| p.map(|p| p.as_ref().to_string()).collect()).unwrap_or_default();
                let key = format!("blobs/{}", rel.join("/"));
                if content_hash(&key, "blobs").is_some() {
                    out.push(object_entry(&meta));
                    if out.len() % 1000 == 0 {
                        seen(out.len() as u64);
                    }
                }
            }
            seen(out.len() as u64);
            Ok(out)
        })
    }

    fn size<'a>(&'a self, hash: &'a str) -> BoxFuture<'a, io::Result<Option<u64>>> {
        Box::pin(async move {
            match self.store.head(&self.blob_key(hash)?).await {
                Ok(meta) => Ok(Some(meta.size)),
                Err(object_store::Error::NotFound { .. }) => Ok(None),
                Err(e) => Err(s3_err(e)),
            }
        })
    }

    fn list(&self) -> BoxFuture<'_, io::Result<Vec<String>>> {
        Box::pin(async move {
            let prefix = self.key("blobs");
            let mut objects = self.store.list(Some(&prefix));
            let mut out = Vec::new();
            while let Some(meta) = objects.next().await {
                let meta = meta.map_err(s3_err)?;
                if let Some(name) = meta.location.filename().filter(|n| is_hash(n)) {
                    out.push(name.to_string());
                }
            }
            Ok(out)
        })
    }

    fn ping(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            // Look up a nonexistent object: a "not found" response means the service works and the keys are valid (costs just one read request)
            match self.store.head(&self.key(".thirtyfile-check/ping")).await {
                Ok(_) | Err(object_store::Error::NotFound { .. }) => Ok(()),
                Err(e) => Err(s3_err(e)),
            }
        })
    }

    fn check(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let key = self.key(&format!(".thirtyfile-check/{}", uuid::Uuid::new_v4().simple()));
            self.store.put(&key, PutPayload::from_static(b"ok")).await.map_err(s3_err)?;
            let back = self.store.get(&key).await.map_err(s3_err)?.bytes().await.map_err(s3_err);
            // Keys that can write but not delete would pass and fail later, when files are deleted
            let deleted = self.delete_object(&key).await;
            if back?.as_ref() != b"ok" {
                return Err(io::Error::other("The content read back didn't match"));
            }
            deleted.map_err(|e| delete_failed(e, CANT_DELETE_S3))
        })
    }
}

/// `https://host[:port]` → `https://{bucket}.host[:port]`
fn virtual_hosted_endpoint(endpoint: &str, bucket: &str) -> io::Result<String> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "Invalid endpoint URL. It should be https://hostname");
    let (scheme, rest) = endpoint.split_once("://").ok_or_else(invalid)?;
    let host = rest.split('/').next().filter(|h| !h.is_empty()).ok_or_else(invalid)?;
    if host.starts_with(&format!("{bucket}.")) {
        return Ok(format!("{scheme}://{host}"));
    }
    Ok(format!("{scheme}://{bucket}.{host}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_in_a_location_stay_inside_it() {
        let h = format!("abcd{}", "0".repeat(60));
        assert_eq!(content_hash(&format!("blobs/ab/cd/{h}"), "blobs"), Some(h.as_str()));
        assert_eq!(content_hash(&format!("ab/cd/{h}"), ""), Some(h.as_str()));
        assert_eq!(content_hash(&format!("blobs/ab/ce/{h}"), "blobs"), None, "not where the store puts it");
        assert_eq!(content_hash(&format!("blobs/AB/CD/{}", h.to_uppercase()), "blobs"), None);
        assert_eq!(content_hash(&format!("other/ab/cd/{h}"), "blobs"), None);
        for bad in ["..", "a/../b", "/etc", "a//b", "a\\b"] {
            assert!(key_parts(bad).is_err(), "{bad}");
        }
        let cfg = S3Config {
            endpoint: "http://127.0.0.1:9000".into(),
            bucket: "files".into(),
            prefix: "/thirtyfile/".into(),
            access_key_id: "key".into(),
            secret_access_key: crate::testutil::password().into(),
            path_style: true,
            allow_http: true,
            ..Default::default()
        };
        let s3 = S3Storage::new(&cfg).unwrap();
        assert_eq!(s3.key_at(".thirtyfile-check/x").unwrap().as_ref(), "thirtyfile/.thirtyfile-check/x");
        assert_eq!(s3.key_at("").unwrap().as_ref(), "thirtyfile");
        assert!(s3.key_at("../other").is_err());
        assert_eq!(s3.content_dir(), "blobs");
    }

    #[test]
    fn virtual_hosted_endpoints() {
        assert_eq!(virtual_hosted_endpoint("https://acct.r2.cloudflarestorage.com", "files").unwrap(), "https://files.acct.r2.cloudflarestorage.com");
        assert_eq!(virtual_hosted_endpoint("http://nas.local:9000", "b").unwrap(), "http://b.nas.local:9000");
        assert_eq!(virtual_hosted_endpoint("https://b.s3.example.com", "b").unwrap(), "https://b.s3.example.com");
        assert!(virtual_hosted_endpoint("s3.example.com", "b").is_err());
    }

    async fn read_all(r: io::Result<BoxReader>) -> Vec<u8> {
        let mut out = Vec::new();
        r.unwrap().read_to_end(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn files_over_16_mib_go_up_in_parts_and_come_back_whole() {
        let s3 = S3Storage::in_memory("tf");
        let dir = std::env::temp_dir().join(format!("thirtyfile-s3-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 17 MB: over the threshold, so a multipart upload of two full parts and a short last one
        let big: Vec<u8> = (0..17_000_000u32).map(|i| (i % 251) as u8).collect();
        assert!(big.len() as u64 > MULTIPART_THRESHOLD);
        let hash = crate::util::sha256_hex(&big);
        let src = dir.join("big");
        std::fs::write(&src, &big).unwrap();
        s3.put_file(&hash, &src).await.unwrap();
        assert!(!src.exists(), "the temp file is removed once stored");

        assert_eq!(s3.size(&hash).await.unwrap(), Some(big.len() as u64));
        let back = read_all(s3.open(&hash, 0, big.len() as u64).await).await;
        assert!(back == big, "the content comes back whole");
        // A range across the boundary between the first two parts
        let at = MULTIPART_CHUNK as u64 - 10;
        assert_eq!(read_all(s3.open(&hash, at, 20).await).await, big[at as usize..at as usize + 20]);
        assert!(read_all(s3.open(&hash, 5, 0).await).await.is_empty());

        // A small file goes up in one request
        let small_hash = crate::util::sha256_hex(b"small");
        std::fs::write(&src, b"small").unwrap();
        s3.put_file(&small_hash, &src).await.unwrap();
        assert_eq!(read_all(s3.open(&small_hash, 0, 5).await).await, b"small");

        // Listings find both under blobs/, in the folders their hash names
        let mut listed = s3.list().await.unwrap();
        listed.sort();
        let mut expected = vec![hash.clone(), small_hash.clone()];
        expected.sort();
        assert_eq!(listed, expected);
        let seen = std::sync::atomic::AtomicU64::new(0);
        let content = s3.list_content(&|n| seen.store(n, std::sync::atomic::Ordering::SeqCst)).await.unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(content.iter().any(|e| e.name == hash && e.size == big.len() as u64 && e.kind == EntryKind::File));
        let top = s3.list_dir("").await.unwrap();
        assert!(top.iter().any(|e| e.name == "blobs" && e.kind == EntryKind::Folder), "{:?}", top.iter().map(|e| &e.name).collect::<Vec<_>>());
        let level = s3.list_dir(&format!("blobs/{}/{}", &hash[0..2], &hash[2..4])).await.unwrap();
        assert!(level.iter().any(|e| e.name == hash && e.kind == EntryKind::File));

        // Deleting, and deleting what is already gone
        s3.delete(&hash).await.unwrap();
        s3.delete(&hash).await.unwrap();
        assert_eq!(s3.size(&hash).await.unwrap(), None);
        assert_eq!(s3.open(&hash, 0, 1).await.err().map(|e| e.kind()), Some(io::ErrorKind::NotFound));
        assert_eq!(s3.list().await.unwrap(), vec![small_hash]);
        assert!(s3.size("not-a-hash").await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn paths_in_a_bucket_and_the_connection_checks() {
        let s3 = S3Storage::in_memory("tf");
        s3.ping().await.unwrap();
        s3.check().await.unwrap();
        // The check leaves nothing behind
        assert!(s3.stat(".thirtyfile-check").await.unwrap().is_none());
        assert!(s3.list_dir(".thirtyfile-check").await.unwrap().is_empty());

        let dir = std::env::temp_dir().join(format!("thirtyfile-s3-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("f");
        std::fs::write(&src, b"hello world").unwrap();
        s3.put_at("backups/one/a.txt", &src).await.unwrap();
        assert!(!src.exists());
        let entry = s3.stat("backups/one/a.txt").await.unwrap().unwrap();
        assert_eq!((entry.name.as_str(), entry.size, entry.kind), ("a.txt", 11, EntryKind::File));
        assert!(entry.modified.is_some());
        assert!(s3.stat("backups/one/b.txt").await.unwrap().is_none());
        assert_eq!(read_all(s3.open_at("backups/one/a.txt", 6, 5).await).await, b"world");
        let listed = s3.list_dir("backups").await.unwrap();
        assert_eq!(listed.iter().map(|e| (e.name.as_str(), e.kind)).collect::<Vec<_>>(), [("one", EntryKind::Folder)]);
        // Kept under the location's prefix, and nothing outside it can be reached
        assert!(s3.store.head(&ObjectPath::from("tf/backups/one/a.txt")).await.is_ok());
        assert!(s3.stat("../a.txt").await.is_err());
        assert!(s3.put_at("a/../../b", &src).await.is_err());
        s3.delete_at("backups/one/a.txt").await.unwrap();
        s3.delete_at("backups/one/a.txt").await.unwrap();
        assert!(s3.stat("backups/one/a.txt").await.unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn part_sizes_stay_under_10000_parts() {
        assert_eq!(part_size(40 << 20), MULTIPART_CHUNK);
        for size in [100u64 << 30, 1 << 40, 4_900u64 << 30] {
            let p = part_size(size) as u64;
            assert!(size.div_ceil(p) <= 10_000 && p.is_multiple_of(1 << 20) && p <= 5 << 30);
        }
    }
}
