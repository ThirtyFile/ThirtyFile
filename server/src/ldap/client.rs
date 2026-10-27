//! An LDAP connection: TLS (LDAPS, or StartTLS on the plain port), a simple bind, and searches that page. It does what a
//! directory sign-in needs and nothing else, and it distrusts what the server sends: a message has a largest size, every
//! read has a time, the entries of a search have a largest number, and referrals are never followed.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use rustls::pki_types::ServerName;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
};
use tokio_rustls::{TlsConnector, client::TlsStream};

use super::{
    ber::{self, APPLICATION, CONSTRUCTED, CONTEXT},
    filter::Filter,
};

/// Largest message taken from a server: a search of many members comes in pages, so no single message is large
const MAX_MESSAGE: usize = 4 * 1024 * 1024;
/// What one search may take from a server in all: pages and bytes
const MAX_PAGES: usize = 2000;
const MAX_RECEIVED: usize = 64 * 1024 * 1024;
/// The OID of the paged results control (RFC 2696), and of StartTLS (RFC 4511 §4.14)
const PAGED_RESULTS: &str = "1.2.840.113556.1.4.319";
const START_TLS: &str = "1.3.6.1.4.1.1466.20037";

/// How the connection is protected
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    /// TLS from the first byte (`ldaps://`)
    Ldaps,
    /// The plain port, then StartTLS before anything is sent
    StartTls,
    /// Nothing: only for a directory on the private network
    None,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// Certificates (DER) that are trusted besides the system's, for a directory with a private authority
    pub roots: Vec<Vec<u8>>,
    /// How long one connection step or one read may take
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The server can't be reached: no address, refused, reset, closed
    Unreachable(String),
    /// The certificate or the handshake isn't right
    Tls(String),
    Timeout,
    /// What the server sent isn't LDAP
    Protocol(String),
    /// More than the limits allow
    TooLarge,
    /// The server answered with an error (`code` is its result code)
    Server {
        code: u32,
        message: String,
    },
    /// Refused before anything was sent
    Invalid(String),
}

impl Error {
    /// The directory can't be used right now, whatever the person typed
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Error::Unreachable(_) | Error::Tls(_) | Error::Timeout | Error::Protocol(_) | Error::TooLarge)
            || matches!(self, Error::Server { code, .. } if matches!(code, 51..=52 | 80))
    }

    /// The password (or the account) is wrong
    pub fn is_bad_credentials(&self) -> bool {
        matches!(self, Error::Server { code: 49, .. })
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unreachable(e) => write!(f, "The directory can't be reached: {e}"),
            Error::Tls(e) => write!(f, "The secure connection failed: {e}"),
            Error::Timeout => write!(f, "The directory took too long to answer"),
            Error::Protocol(e) => write!(f, "The answer isn't LDAP: {e}"),
            Error::TooLarge => write!(f, "The directory's answer was larger than allowed"),
            Error::Server { code, message } if message.is_empty() => write!(f, "The directory refused it ({})", result_name(*code)),
            Error::Server { code, message } => write!(f, "The directory refused it ({}): {message}", result_name(*code)),
            Error::Invalid(e) => write!(f, "{e}"),
        }
    }
}

/// What a result code is called
pub fn result_name(code: u32) -> String {
    match code {
        0 => "success".into(),
        1 => "operations error".into(),
        2 => "protocol error".into(),
        3 => "time limit exceeded".into(),
        4 => "size limit exceeded".into(),
        7 => "authentication method not supported".into(),
        8 => "strong authentication required".into(),
        10 => "referral".into(),
        11 => "administrative limit exceeded".into(),
        32 => "no such object".into(),
        34 => "invalid DN syntax".into(),
        48 => "inappropriate authentication".into(),
        49 => "invalid credentials".into(),
        50 => "insufficient access rights".into(),
        51 => "busy".into(),
        52 => "unavailable".into(),
        53 => "unwilling to perform".into(),
        c => format!("code {c}"),
    }
}

fn io_error(e: io::Error) -> Error {
    Error::Unreachable(e.to_string())
}

// ───────────── The wire ─────────────

enum Transport {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
    /// While the connection is being made secure
    Moved,
}

impl AsyncRead for Transport {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Transport::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
            Transport::Moved => Poll::Ready(Err(io::ErrorKind::NotConnected.into())),
        }
    }
}

impl AsyncWrite for Transport {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Transport::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
            Transport::Moved => Poll::Ready(Err(io::ErrorKind::NotConnected.into())),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_flush(cx),
            Transport::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
            Transport::Moved => Poll::Ready(Ok(())),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Transport::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Transport::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
            Transport::Moved => Poll::Ready(Ok(())),
        }
    }
}

/// One message from the server: its id, the protocol operation (tag and content) and its controls
struct Message {
    id: i64,
    op: u8,
    body: Vec<u8>,
    controls: Vec<u8>,
}

/// An LDAP result: the code and what the server said
struct Outcome {
    code: u32,
    message: String,
}

fn outcome(body: &[u8]) -> Result<Outcome, Error> {
    let bad = |_| Error::Protocol("a result that is cut short".into());
    let parts = ber::items(body).map_err(bad)?;
    let (Some(code), Some(_matched), Some(message)) = (parts.first(), parts.get(1), parts.get(2)) else {
        return Err(Error::Protocol("a result that is cut short".into()));
    };
    Ok(Outcome { code: u32::try_from(code.int().map_err(bad)?).unwrap_or(u32::MAX), message: message.text() })
}

/// What a search asks
#[derive(Debug, Clone)]
pub struct Search {
    pub base: String,
    pub scope: Scope,
    pub filter: Filter,
    pub attributes: Vec<String>,
    /// The most entries the server is asked for (0 = no limit asked)
    pub size_limit: u32,
    /// Entries per page of a search that pages (0 = one answer)
    pub page_size: u32,
    /// The most entries taken over all pages; more is an error
    pub max_entries: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Subtree = 2,
}

/// An entry of the directory: its name and its attributes
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub dn: String,
    pub attributes: Vec<(String, Vec<Vec<u8>>)>,
}

impl Entry {
    /// The values of an attribute (its name is compared without regard to case)
    pub fn values(&self, name: &str) -> &[Vec<u8>] {
        self.attributes.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map_or(&[], |(_, v)| v.as_slice())
    }

    pub fn bytes(&self, name: &str) -> Option<&[u8]> {
        self.values(name).first().map(Vec::as_slice)
    }

    pub fn text(&self, name: &str) -> Option<String> {
        self.bytes(name).map(|b| String::from_utf8_lossy(b).into_owned())
    }

    pub fn texts(&self, name: &str) -> Vec<String> {
        self.values(name).iter().map(|b| String::from_utf8_lossy(b).into_owned()).collect()
    }
}

/// What a search found; `truncated`: the server stopped at the size limit that was asked
#[derive(Debug, Default)]
pub struct Found {
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

pub struct Connection {
    io: Transport,
    next_id: i64,
    timeout: Duration,
}

impl Connection {
    /// Connects and makes the connection secure as `config` says
    pub async fn connect(config: &Config) -> Result<Connection, Error> {
        let step = config.timeout;
        let addrs = tokio::time::timeout(step, tokio::net::lookup_host((config.host.as_str(), config.port))).await.map_err(|_| Error::Timeout)?.map_err(io_error)?;
        let mut last = Error::Unreachable(format!("no address for {}", config.host));
        let mut tcp = None;
        for addr in addrs {
            match tokio::time::timeout(step, TcpStream::connect(addr)).await {
                Ok(Ok(s)) => {
                    tcp = Some(s);
                    break;
                }
                Ok(Err(e)) => last = io_error(e),
                Err(_) => last = Error::Timeout,
            }
        }
        let tcp = tcp.ok_or(last)?;
        let _ = tcp.set_nodelay(true);
        let mut conn = Connection { io: Transport::Plain(tcp), next_id: 1, timeout: step };
        match config.security {
            Security::Ldaps => conn.secure(config).await?,
            Security::StartTls => {
                conn.start_tls(config).await?;
            }
            Security::None => {}
        }
        Ok(conn)
    }

    async fn secure(&mut self, config: &Config) -> Result<(), Error> {
        let Transport::Plain(tcp) = std::mem::replace(&mut self.io, Transport::Moved) else { return Err(Error::Tls("already secure".into())) };
        let tls = crate::tls::client_tls_with_roots(&config.roots).map_err(|e| Error::Tls(e.to_string()))?;
        let name = ServerName::try_from(config.host.clone()).map_err(|_| Error::Tls(format!("{} isn't a host name or an address", config.host)))?;
        let stream = tokio::time::timeout(self.timeout, TlsConnector::from(tls).connect(name, tcp))
            .await
            .map_err(|_| Error::Timeout)?
            .map_err(|e| Error::Tls(tls_reason(&e)))?;
        self.io = Transport::Tls(Box::new(stream));
        Ok(())
    }

    /// StartTLS (RFC 4511 §4.14): asks, and once the server agrees, the handshake follows on the same connection
    async fn start_tls(&mut self, config: &Config) -> Result<(), Error> {
        let request = ber::constructed(APPLICATION | CONSTRUCTED | 23, &[ber::octets(CONTEXT, START_TLS.as_bytes())]);
        let id = self.send(request, None).await?;
        let reply = self.read_for(id).await?;
        if reply.op != APPLICATION | CONSTRUCTED | 24 {
            return Err(Error::Protocol("an answer to StartTLS that isn't one".into()));
        }
        let result = outcome(&reply.body)?;
        if result.code != 0 {
            return Err(Error::Server { code: result.code, message: result.message });
        }
        self.secure(config).await
    }

    /// A simple bind as `dn` with `password`. An empty password is refused here: servers take a bind without one as
    /// anonymous and say it worked, which would let anyone in as anyone (RFC 4513 §5.1.2).
    pub async fn bind(&mut self, dn: &str, password: &str) -> Result<(), Error> {
        if password.is_empty() {
            return Err(Error::Invalid("A password is needed".into()));
        }
        if dn.is_empty() {
            return Err(Error::Invalid("A bind needs the name of an entry".into()));
        }
        let request = ber::constructed(
            APPLICATION | CONSTRUCTED,
            &[ber::int(ber::INTEGER, 3), ber::octets(ber::OCTET_STRING, dn.as_bytes()), ber::octets(CONTEXT, password.as_bytes())],
        );
        let id = self.send(request, None).await?;
        let reply = self.read_for(id).await?;
        if reply.op != APPLICATION | CONSTRUCTED | 1 {
            return Err(Error::Protocol("an answer to a bind that isn't one".into()));
        }
        let result = outcome(&reply.body)?;
        if result.code != 0 {
            return Err(Error::Server { code: result.code, message: result.message });
        }
        Ok(())
    }

    /// Searches; a search that pages asks for one page at a time. The whole of it has a time, as each read has: a server
    /// that answers a little at a time can't keep the search going for ever.
    pub async fn search(&mut self, search: &Search) -> Result<Found, Error> {
        let whole = self.timeout * 12;
        tokio::time::timeout(whole, self.search_pages(search)).await.map_err(|_| Error::Timeout)?
    }

    async fn search_pages(&mut self, search: &Search) -> Result<Found, Error> {
        let mut found = Found::default();
        let mut cookie: Vec<u8> = Vec::new();
        let (mut received, mut pages) = (0usize, 0usize);
        loop {
            pages += 1;
            if pages > MAX_PAGES {
                return Err(Error::TooLarge);
            }
            let request = ber::constructed(
                APPLICATION | CONSTRUCTED | 3,
                &[
                    ber::octets(ber::OCTET_STRING, search.base.as_bytes()),
                    ber::int(ber::ENUMERATED, search.scope as i64),
                    // Aliases are never followed
                    ber::int(ber::ENUMERATED, 0),
                    ber::int(ber::INTEGER, i64::from(search.size_limit)),
                    ber::int(ber::INTEGER, self.timeout.as_secs().max(1) as i64),
                    ber::boolean(ber::BOOLEAN, false),
                    search.filter.to_ber(),
                    ber::constructed(ber::SEQUENCE, &search.attributes.iter().map(|a| ber::octets(ber::OCTET_STRING, a.as_bytes())).collect::<Vec<_>>()),
                ],
            );
            let controls = (search.page_size > 0).then(|| paged_control(search.page_size, &cookie));
            let id = self.send(request, controls).await?;
            let next = loop {
                let message = self.read_for(id).await?;
                received += message.body.len();
                if received > MAX_RECEIVED {
                    return Err(Error::TooLarge);
                }
                match message.op {
                    op if op == APPLICATION | CONSTRUCTED | 4 => {
                        if found.entries.len() >= search.max_entries {
                            return Err(Error::TooLarge);
                        }
                        found.entries.push(entry(&message.body)?);
                    }
                    // References to other servers are not followed; what the server says on the way is not needed
                    op if op == APPLICATION | CONSTRUCTED | 19 || op == APPLICATION | 25 || op == APPLICATION | CONSTRUCTED | 25 => {}
                    op if op == APPLICATION | CONSTRUCTED | 5 => {
                        let result = outcome(&message.body)?;
                        match result.code {
                            0 => {}
                            // The server stopped at the limit that was asked for
                            4 if search.size_limit > 0 => found.truncated = true,
                            code => return Err(Error::Server { code, message: result.message }),
                        }
                        break next_cookie(&message.controls);
                    }
                    _ => return Err(Error::Protocol("a message that isn't part of the search".into())),
                }
            };
            match next {
                Some(c) if search.page_size > 0 && !c.is_empty() && !found.truncated => cookie = c,
                _ => return Ok(found),
            }
        }
    }

    /// Says goodbye and closes
    pub async fn unbind(mut self) {
        let request = ber::tlv(APPLICATION | 2, &[]);
        let _ = self.send(request, None).await;
        let _ = tokio::time::timeout(Duration::from_secs(1), self.io.shutdown()).await;
    }

    async fn send(&mut self, op: Vec<u8>, controls: Option<Vec<u8>>) -> Result<i64, Error> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let mut parts = vec![ber::int(ber::INTEGER, id), op];
        parts.extend(controls);
        let message = ber::constructed(ber::SEQUENCE, &parts);
        tokio::time::timeout(self.timeout, async {
            self.io.write_all(&message).await?;
            self.io.flush().await
        })
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(io_error)?;
        Ok(id)
    }

    /// The next message, which has to answer request `id`
    async fn read_for(&mut self, id: i64) -> Result<Message, Error> {
        let message = self.read_message().await?;
        // Unsolicited notices (id 0: the server is going away) and answers to other requests aren't ones this client expects
        if message.id != id {
            return Err(Error::Protocol(if message.id == 0 { "the server closed the connection".into() } else { "an answer to another request".into() }));
        }
        Ok(message)
    }

    async fn read_message(&mut self) -> Result<Message, Error> {
        let timeout = self.timeout;
        let mut head = [0u8; 2];
        read_exact(&mut self.io, &mut head, timeout).await?;
        if head[0] != ber::SEQUENCE {
            return Err(Error::Protocol("a message that doesn't begin as LDAP does".into()));
        }
        let mut raw = head.to_vec();
        let len = if head[1] & 0x80 == 0 {
            usize::from(head[1])
        } else {
            let n = usize::from(head[1] & 0x7f);
            if n == 0 || n > 4 {
                return Err(Error::Protocol("a message with a length LDAP doesn't use".into()));
            }
            let mut bytes = [0u8; 4];
            read_exact(&mut self.io, &mut bytes[..n], timeout).await?;
            raw.extend_from_slice(&bytes[..n]);
            bytes[..n].iter().fold(0usize, |acc, b| (acc << 8) | usize::from(*b))
        };
        // Refused before anything is allocated for it
        if len > MAX_MESSAGE {
            return Err(Error::TooLarge);
        }
        let start = raw.len();
        raw.resize(start + len, 0);
        read_exact(&mut self.io, &mut raw[start..], timeout).await?;
        let protocol = |_| Error::Protocol("a message that is cut short".into());
        let (outer, _) = ber::read(&raw).map_err(protocol)?;
        let parts = ber::items(outer.content).map_err(protocol)?;
        let (Some(id), Some(op)) = (parts.first(), parts.get(1)) else { return Err(Error::Protocol("a message that is cut short".into())) };
        let controls = parts.get(2).filter(|c| c.tag == CONTEXT | CONSTRUCTED).map(|c| c.content.to_vec()).unwrap_or_default();
        Ok(Message { id: id.int().map_err(protocol)?, op: op.tag, body: op.content.to_vec(), controls })
    }
}

async fn read_exact(io: &mut Transport, buf: &mut [u8], timeout: Duration) -> Result<(), Error> {
    match tokio::time::timeout(timeout, io.read_exact(buf)).await {
        Err(_) => Err(Error::Timeout),
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) if e.kind() == io::ErrorKind::UnexpectedEof => Err(Error::Unreachable("the connection was closed".into())),
        Ok(Err(e)) => Err(io_error(e)),
    }
}

/// What went wrong in a handshake, in words: an unknown authority and a wrong name are what people meet
fn tls_reason(e: &io::Error) -> String {
    let text = e.to_string();
    if let Some(inner) = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()) {
        return match inner {
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer) => {
                "the certificate was issued by an authority that isn't trusted: add its certificate".into()
            }
            rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForName)
            | rustls::Error::InvalidCertificate(rustls::CertificateError::NotValidForNameContext { .. }) => "the certificate isn't for this server's name".into(),
            rustls::Error::InvalidCertificate(rustls::CertificateError::Expired)
            | rustls::Error::InvalidCertificate(rustls::CertificateError::ExpiredContext { .. }) => "the certificate has expired".into(),
            other => other.to_string(),
        };
    }
    text
}

fn paged_control(size: u32, cookie: &[u8]) -> Vec<u8> {
    let value = ber::constructed(ber::SEQUENCE, &[ber::int(ber::INTEGER, i64::from(size)), ber::octets(ber::OCTET_STRING, cookie)]);
    let control = ber::constructed(
        ber::SEQUENCE,
        &[ber::octets(ber::OCTET_STRING, PAGED_RESULTS.as_bytes()), ber::boolean(ber::BOOLEAN, false), ber::octets(ber::OCTET_STRING, &value)],
    );
    ber::constructed(CONTEXT | CONSTRUCTED, &[control])
}

/// The cookie of the next page, when the controls of a search's end hold one
fn next_cookie(controls: &[u8]) -> Option<Vec<u8>> {
    for control in ber::items(controls).ok()? {
        let parts = ber::items(control.content).ok()?;
        if parts.first().map(|p| p.content) != Some(PAGED_RESULTS.as_bytes()) {
            continue;
        }
        let value = parts.last().filter(|p| p.tag == ber::OCTET_STRING)?;
        let (seq, _) = ber::read(value.content).ok()?;
        let inner = ber::items(seq.content).ok()?;
        return inner.get(1).map(|c| c.content.to_vec());
    }
    None
}

fn entry(body: &[u8]) -> Result<Entry, Error> {
    let bad = |_| Error::Protocol("an entry that is cut short".into());
    let parts = ber::items(body).map_err(bad)?;
    let (Some(dn), Some(attrs)) = (parts.first(), parts.get(1)) else { return Err(Error::Protocol("an entry that is cut short".into())) };
    let mut attributes = Vec::new();
    for attr in ber::items(attrs.content).map_err(bad)? {
        let pair = ber::items(attr.content).map_err(bad)?;
        let (Some(name), Some(values)) = (pair.first(), pair.get(1)) else { return Err(Error::Protocol("an entry that is cut short".into())) };
        attributes.push((name.text(), ber::items(values.content).map_err(bad)?.into_iter().map(|v| v.content.to_vec()).collect()));
    }
    Ok(Entry { dn: dn.text(), attributes })
}
