//! S3-compatible object storage

use thirtys3_client::{Addressing, Client, ClientConfig, Credentials, GetOptions, GetRange, ListOptions, ObjectMeta, PutOptions};

use super::*;

pub struct S3Storage {
    client: Client,
    /// The settings the client was made from (a file too big for the usual part size is sent by a client with a bigger one)
    settings: ClientConfig,
    prefix: String,
    /// The service the tests run against, to look at what it holds
    #[cfg(test)]
    memory: Option<thirtys3_server::memory::MemoryBackend>,
}

#[cfg(test)]
pub(crate) const TEST_BUCKET: &str = "test";

/// A service in memory on this computer (tests; needs a running Tokio runtime): what it holds, where it is, and the secret of its keys `test-key`
/// (may write) and `read-key` (may only read)
#[cfg(test)]
pub(crate) fn serve_in_memory() -> (thirtys3_server::memory::MemoryBackend, std::net::SocketAddr, String) {
    use thirtys3_server::{S3Service, ServerConfig, memory::MemoryBackend};
    let secret = crate::testutil::password();
    let backend = MemoryBackend::new().with_bucket(TEST_BUCKET).with_key("test-key", secret).with_read_only_key("read-key", secret);
    let service = S3Service::new(backend.clone(), ServerConfig::default());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let service = service.clone();
            tokio::spawn(async move {
                let hyper_service = hyper::service::service_fn(move |request: axum::http::Request<hyper::body::Incoming>| {
                    let service = service.clone();
                    async move { Ok::<_, std::convert::Infallible>(service.handle(request).await) }
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(hyper_util::rt::TokioIo::new(stream), hyper_service).await;
            });
        }
    });
    (backend, address, secret.to_string())
}

#[cfg(test)]
impl S3Storage {
    /// The settings for the service of `serve_in_memory`, with the key `key`
    pub(crate) fn test_config(address: std::net::SocketAddr, key: &str, secret: &str, prefix: &str) -> S3Config {
        S3Config {
            endpoint: format!("http://{address}"),
            bucket: TEST_BUCKET.into(),
            prefix: prefix.into(),
            access_key_id: key.into(),
            secret_access_key: secret.into(),
            path_style: true,
            allow_http: true,
            ..Default::default()
        }
    }

    /// A bucket kept in memory by a service on this computer, reached through the same code (and the same signing) as a real one (tests; needs a
    /// running Tokio runtime)
    pub fn in_memory(prefix: &str) -> Self {
        let (backend, address, secret) = serve_in_memory();
        Self { memory: Some(backend), ..Self::new(&Self::test_config(address, "test-key", &secret, prefix)).unwrap() }
    }
}

/// Connect timeout: report quickly when the service can't be reached at all
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Read timeout: a connection counts as stuck only after receiving no data for this long (the total transfer time isn't limited, so large files and slow downloads aren't interrupted)
const READ_TIMEOUT: Duration = Duration::from_secs(15);
/// Retry count: when the service doesn't respond, users see an error after about 45 seconds instead of waiting several minutes
const MAX_RETRIES: u32 = 2;
/// How long a download that broke off is resumed without receiving anything new (a download that keeps moving is never cut off by it)
const RETRY_WINDOW: Duration = Duration::from_secs(30 * 60);

const MULTIPART_THRESHOLD: u64 = 16 * 1024 * 1024;
const MULTIPART_CHUNK: usize = 8 * 1024 * 1024;

/// S3 / R2 allow at most 10,000 parts: starting at 8 MiB, larger files use larger parts (in whole MiB, all parts the same size)
fn part_size(size: u64) -> usize {
    const MIB: u64 = 1024 * 1024;
    let needed = size.div_ceil(9_000).div_ceil(MIB) * MIB;
    needed.max(MULTIPART_CHUNK as u64) as usize
}

fn s3_err(e: thirtys3_client::Error) -> io::Error {
    use thirtys3_client::ErrorKind;
    let message = match e.kind() {
        ErrorKind::NotFound => return io::Error::new(io::ErrorKind::NotFound, e.to_string()),
        ErrorKind::PermissionDenied => DENIED_KEYS,
        _ => UNAVAILABLE,
    };
    io::Error::other(StorageError { message, detail: e.to_string() })
}

/// The last part of a key
fn file_name(key: &str) -> &str {
    key.trim_end_matches('/').rsplit('/').next().unwrap_or_default()
}

fn object_entry(meta: &ObjectMeta) -> Entry {
    Entry::file(file_name(&meta.key), meta.size, meta.last_modified.map(|t| t.0))
}

impl S3Storage {
    pub fn new(cfg: &S3Config) -> io::Result<Self> {
        let bucket = cfg.bucket.trim();
        let endpoint = cfg.endpoint.trim().trim_end_matches('/');
        let credentials = Credentials::new(cfg.access_key_id.trim(), cfg.secret_access_key.trim());
        let mut settings = ClientConfig::new(bucket, credentials).region(cfg.region.trim());
        settings.allow_http = cfg.allow_http;
        // Without "path style" the bucket is in the host name. The client still uses path style for a bucket with a "." in its name (the wildcard
        // certificate of AWS covers one level of names only) and for an address that is an IP address
        settings.addressing = if cfg.path_style { Addressing::PathStyle } else { Addressing::VirtualHosted };
        settings.connect_timeout = CONNECT_TIMEOUT;
        settings.read_timeout = READ_TIMEOUT;
        settings.max_retries = MAX_RETRIES;
        settings.retry_timeout = RETRY_WINDOW;
        // We delete one object at a time; single DELETE is a basic API every S3-compatible service supports (some lack DeleteObjects)
        settings.bulk_delete = false;
        if !endpoint.is_empty() {
            settings.endpoint = Some(if cfg.path_style { endpoint.to_string() } else { endpoint_without_bucket(endpoint, bucket)? });
        }
        let client = Client::new(settings.clone()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        Ok(Self {
            client,
            settings,
            prefix: cfg.prefix.trim().trim_matches('/').to_string(),
            #[cfg(test)]
            memory: None,
        })
    }

    fn key(&self, name: &str) -> String {
        join_key(&[&self.prefix, name])
    }

    fn blob_key(&self, hash: &str) -> io::Result<String> {
        valid_hash(hash)?;
        Ok(self.key(&format!("blobs/{}/{}/{hash}", &hash[0..2], &hash[2..4])))
    }

    /// A path in the location as an object key (a checked path; "" is the prefix itself)
    fn key_at(&self, key: &str) -> io::Result<String> {
        key_parts(key)?;
        Ok(join_key(&[&self.prefix, key]))
    }

    /// Stores a temp file at `key`: one request up to 16 MiB, a multipart upload above (the temp file is kept)
    async fn put_object(&self, key: &str, src: &Path) -> io::Result<()> {
        let size = tokio::fs::metadata(src).await?.len();
        let options = PutOptions::default();
        if size <= MULTIPART_THRESHOLD {
            let data = tokio::fs::read(src).await?;
            self.client.put(key, data.into(), &options).await.map_err(s3_err)?;
            return Ok(());
        }
        // Every part (except the last) has the same size: R2 requires it and AWS accepts it. A failed upload is aborted by the client, so unfinished
        // parts don't stay in the bucket and keep incurring charges
        let chunk = part_size(size);
        let file = tokio::fs::File::open(src).await?;
        if chunk == MULTIPART_CHUNK {
            self.client.put_multipart(key, file, &options).await.map_err(s3_err)?;
        } else {
            Client::new(self.upload_settings(chunk)).map_err(s3_err)?.put_multipart(key, file, &options).await.map_err(s3_err)?;
        }
        Ok(())
    }

    /// The settings for a client that sends parts of `chunk` bytes: with big parts fewer are in flight at once, to keep the memory used the same
    fn upload_settings(&self, chunk: usize) -> ClientConfig {
        let mut settings = self.settings.clone();
        settings.part_size = chunk;
        settings.multipart_concurrency = if chunk > 64 << 20 { 2 } else { 4 };
        settings
    }

    async fn open_object(&self, key: &str, start: u64, len: u64) -> io::Result<BoxReader> {
        if len == 0 {
            return Ok(Box::pin(tokio::io::empty()) as BoxReader);
        }
        let options = GetOptions { range: Some(GetRange::Bounded(start..start + len)), ..GetOptions::default() };
        let result = self.client.get(key, &options).await.map_err(s3_err)?;
        Ok(Box::pin(StreamReader::new(result.body.map_err(s3_err))) as BoxReader)
    }

    /// Deleting what is already gone is not an error
    async fn delete_object(&self, key: &str) -> io::Result<()> {
        self.client.delete(key).await.map_err(s3_err)
    }

    /// The metadata of an object, None when it isn't there
    async fn head(&self, key: &str) -> io::Result<Option<ObjectMeta>> {
        match self.client.head(key).await {
            Ok(meta) => Ok(Some(meta)),
            Err(e) if e.is_not_found() => Ok(None),
            Err(e) => Err(s3_err(e)),
        }
    }
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
            let key = self.key_at(dir)?;
            let prefix = if key.is_empty() { key } else { format!("{key}/") };
            let listed = self.client.list_with_delimiter(&prefix, "/").await.map_err(s3_err)?;
            let folders = listed.common_prefixes.iter().map(|p| file_name(p)).filter(|name| !name.is_empty()).map(|name| Entry {
                name: name.to_string(),
                kind: EntryKind::Folder,
                size: 0,
                modified: None,
            });
            // An object whose key ends in "/" is a folder marker some tools leave: it has no name here
            Ok(folders.chain(listed.objects.iter().filter(|meta| !meta.key.ends_with('/')).map(object_entry)).collect())
        })
    }

    fn stat<'a>(&'a self, key: &'a str) -> BoxFuture<'a, io::Result<Option<Entry>>> {
        Box::pin(async move { Ok(self.head(&self.key_at(key)?).await?.map(|meta| object_entry(&meta))) })
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
            let prefix = format!("{}/", self.key("blobs"));
            let options = ListOptions::default();
            let mut objects = self.client.list(&prefix, &options);
            let mut out = Vec::new();
            while let Some(meta) = objects.next().await {
                let meta = meta.map_err(s3_err)?;
                let Some(rest) = meta.key.strip_prefix(&prefix) else { continue };
                if content_hash(&format!("blobs/{rest}"), "blobs").is_some() {
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
        Box::pin(async move { Ok(self.head(&self.blob_key(hash)?).await?.map(|meta| meta.size)) })
    }

    fn list(&self) -> BoxFuture<'_, io::Result<Vec<String>>> {
        Box::pin(async move {
            let prefix = format!("{}/", self.key("blobs"));
            let options = ListOptions::default();
            let mut objects = self.client.list(&prefix, &options);
            let mut out = Vec::new();
            while let Some(meta) = objects.next().await {
                let meta = meta.map_err(s3_err)?;
                if let Some(name) = Some(file_name(&meta.key)).filter(|n| is_hash(n)) {
                    out.push(name.to_string());
                }
            }
            Ok(out)
        })
    }

    fn ping(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            // Look up a nonexistent object: a "not found" response means the service works and the keys are valid (costs just one read request)
            self.head(&self.key(".thirtyfile-check/ping")).await.map(|_| ())
        })
    }

    fn check(&self) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            let key = self.key(&format!(".thirtyfile-check/{}", uuid::Uuid::new_v4().simple()));
            self.client.put(&key, bytes::Bytes::from_static(b"ok"), &PutOptions::default()).await.map_err(s3_err)?;
            let back = match self.client.get(&key, &GetOptions::default()).await {
                Ok(result) => result.body.bytes().await.map_err(s3_err),
                Err(e) => Err(s3_err(e)),
            };
            // Keys that can write but not delete would pass and fail later, when files are deleted
            let deleted = self.delete_object(&key).await;
            if back?.as_ref() != b"ok" {
                return Err(io::Error::other("The content read back didn't match"));
            }
            deleted.map_err(|e| delete_failed(e, CANT_DELETE_S3))
        })
    }
}

/// `https://bucket.host[:port]` → `https://host[:port]`: the client puts the bucket in front of the host itself (an address that has it already is
/// the same address); an address that is not `scheme://host` is refused
fn endpoint_without_bucket(endpoint: &str, bucket: &str) -> io::Result<String> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "Invalid endpoint URL. It should be https://hostname");
    let (scheme, rest) = endpoint.split_once("://").ok_or_else(invalid)?;
    let host = rest.split('/').next().filter(|h| !h.is_empty()).ok_or_else(invalid)?;
    Ok(format!("{scheme}://{}", host.strip_prefix(&format!("{bucket}.")).unwrap_or(host)))
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
        assert_eq!(s3.key_at(".thirtyfile-check/x").unwrap(), "thirtyfile/.thirtyfile-check/x");
        assert_eq!(s3.key_at("").unwrap(), "thirtyfile");
        assert!(s3.key_at("../other").is_err());
        assert_eq!(s3.content_dir(), "blobs");
    }

    #[test]
    fn endpoints_name_the_service_and_not_the_bucket() {
        assert_eq!(endpoint_without_bucket("https://acct.r2.cloudflarestorage.com", "files").unwrap(), "https://acct.r2.cloudflarestorage.com");
        assert_eq!(endpoint_without_bucket("http://nas.local:9000", "b").unwrap(), "http://nas.local:9000");
        assert_eq!(endpoint_without_bucket("https://b.s3.example.com", "b").unwrap(), "https://s3.example.com");
        assert_eq!(endpoint_without_bucket("https://b.s3.example.com:8443/b", "b").unwrap(), "https://s3.example.com:8443");
        assert!(endpoint_without_bucket("s3.example.com", "b").is_err());
        assert!(endpoint_without_bucket("https://", "b").is_err());
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
        assert_eq!(s3.memory.as_ref().unwrap().open_uploads(), 0, "no upload is left open");

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
        assert!(s3.memory.as_ref().unwrap().keys(TEST_BUCKET).is_empty());

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
        assert!(s3.memory.as_ref().unwrap().object(TEST_BUCKET, "tf/backups/one/a.txt").is_some());
        assert!(s3.stat("../a.txt").await.is_err());
        assert!(s3.put_at("a/../../b", &src).await.is_err());
        s3.delete_at("backups/one/a.txt").await.unwrap();
        s3.delete_at("backups/one/a.txt").await.unwrap();
        assert!(s3.stat("backups/one/a.txt").await.unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn big_parts_are_sent_with_fewer_at_once_and_the_rest_of_the_settings_stay() {
        let s3 = S3Storage::in_memory("tf");
        let small = s3.upload_settings(10 << 20);
        let big = s3.upload_settings(100 << 20);
        assert_eq!((small.part_size, small.multipart_concurrency), (10 << 20, 4));
        assert_eq!((big.part_size, big.multipart_concurrency), (100 << 20, 2));
        assert_eq!((big.connect_timeout, big.read_timeout, big.max_retries, big.retry_timeout), (CONNECT_TIMEOUT, READ_TIMEOUT, MAX_RETRIES, RETRY_WINDOW));
        assert!(!big.bulk_delete && big.allow_http && big.bucket == TEST_BUCKET);
    }

    #[test]
    fn part_sizes_stay_under_10000_parts() {
        assert_eq!(part_size(40 << 20), MULTIPART_CHUNK);
        for size in [100u64 << 30, 1 << 40, 4_900u64 << 30] {
            let p = part_size(size) as u64;
            assert!(size.div_ceil(p) <= 10_000 && p.is_multiple_of(1 << 20) && p <= 5 << 30);
        }
    }

    #[tokio::test]
    async fn names_with_spaces_signs_and_other_scripts_come_back_as_they_went() {
        let s3 = S3Storage::in_memory("tf");
        let dir = std::env::temp_dir().join(format!("thirtyfile-s3-{}", crate::util::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("f");
        let names = ["my file (1).txt", "a+b=c&d%20e.txt", "報告 2026 年.docx", "emoji 🙂.txt", "semi;colon,comma'quote.txt", "tilde~dot..txt"];
        for name in names {
            std::fs::write(&src, name.as_bytes()).unwrap();
            s3.put_at(&format!("dir with space/{name}"), &src).await.unwrap();
        }
        let mut listed: Vec<String> = s3.list_dir("dir with space").await.unwrap().into_iter().map(|e| e.name).collect();
        listed.sort();
        let mut expected: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        expected.sort();
        assert_eq!(listed, expected);
        for name in names {
            let key = format!("dir with space/{name}");
            assert_eq!(s3.stat(&key).await.unwrap().map(|e| e.name), Some(name.to_string()));
            assert_eq!(read_all(s3.open_at(&key, 0, 100).await).await, name.as_bytes());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn folder_markers_left_by_other_tools_are_not_files() {
        let s3 = S3Storage::in_memory("tf");
        let memory = s3.memory.as_ref().unwrap();
        memory.insert(TEST_BUCKET, "tf/photos/", b"");
        memory.insert(TEST_BUCKET, "tf/photos/a.jpg", b"jpeg");
        let listed = s3.list_dir("").await.unwrap();
        assert_eq!(listed.iter().map(|e| (e.name.as_str(), e.kind)).collect::<Vec<_>>(), [("photos", EntryKind::Folder)]);
        let inside = s3.list_dir("photos").await.unwrap();
        assert_eq!(inside.iter().map(|e| (e.name.as_str(), e.kind, e.size)).collect::<Vec<_>>(), [("a.jpg", EntryKind::File, 4)]);
    }

    #[tokio::test]
    async fn the_keys_and_the_service_decide_what_the_error_says() {
        let (_backend, address, secret) = serve_in_memory();
        let message = |e: io::Error| e.get_ref().and_then(|inner| inner.downcast_ref::<StorageError>()).map(|e| e.message);
        // Keys the service doesn't know, a wrong secret, and keys that may only read
        let unknown = S3Storage::new(&S3Storage::test_config(address, "no-such-key", &secret, "tf")).unwrap();
        assert_eq!(message(unknown.check().await.unwrap_err()), Some(DENIED_KEYS));
        let wrong = S3Storage::new(&S3Storage::test_config(address, "test-key", "not the secret", "tf")).unwrap();
        assert_eq!(message(wrong.ping().await.unwrap_err()), Some(DENIED_KEYS));
        let reader = S3Storage::new(&S3Storage::test_config(address, "read-key", &secret, "tf")).unwrap();
        reader.ping().await.unwrap();
        assert_eq!(message(reader.check().await.unwrap_err()), Some(DENIED_KEYS));
        // A bucket that isn't there is "not found", not a refusal
        let mut cfg = S3Storage::test_config(address, "test-key", &secret, "tf");
        cfg.bucket = "no-such-bucket".into();
        let missing = S3Storage::new(&cfg).unwrap();
        assert_eq!(missing.check().await.unwrap_err().kind(), io::ErrorKind::NotFound);
        // Nothing listening: the storage service is "unavailable", found without a long wait
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let nowhere = closed.local_addr().unwrap();
        drop(closed);
        let unreachable = S3Storage::new(&S3Storage::test_config(nowhere, "test-key", &secret, "tf")).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(message(unreachable.ping().await.unwrap_err()), Some(UNAVAILABLE));
        assert!(started.elapsed() < Duration::from_secs(20), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn the_bucket_may_be_in_the_host_name_or_in_the_path() {
        let (_backend, address, secret) = serve_in_memory();
        // The service is reached by its IP address, so even "virtual-hosted" settings use the path
        let mut cfg = S3Storage::test_config(address, "test-key", &secret, "tf");
        cfg.path_style = false;
        S3Storage::new(&cfg).unwrap().check().await.unwrap();
        cfg.endpoint = format!("http://{TEST_BUCKET}.{address}");
        assert!(S3Storage::new(&cfg).is_ok());
        cfg.endpoint = "127.0.0.1:9000".into();
        assert!(S3Storage::new(&cfg).is_err(), "an endpoint without http:// or https:// is refused");
    }
}
