//! A directory for the tests: a server on a local port that speaks enough LDAP to answer binds and searches from a few
//! entries, with the ways a real one fails (slow, silent, not LDAP at all, an answer too large), and TLS or StartTLS
//! with a certificate made for the test. What it was asked is kept, so a test can see what the client sent.

use std::{collections::HashMap, sync::Arc, time::Duration};

use crate::sync::Mutex;
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
};

use super::{
    ber::{self, APPLICATION, CONSTRUCTED, CONTEXT},
    client::Config,
    filter::Filter,
};

#[derive(Clone, Default)]
pub struct Person {
    pub dn: String,
    pub attributes: Vec<(String, Vec<String>)>,
}

impl Person {
    pub fn new(dn: &str, attributes: &[(&str, &[&str])]) -> Person {
        Person { dn: dn.into(), attributes: attributes.iter().map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect())).collect() }
    }

    fn values(&self, name: &str) -> Vec<&str> {
        self.attributes.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).flat_map(|(_, v)| v.iter().map(String::as_str)).collect()
    }
}

#[derive(Clone, Default)]
pub struct Directory {
    pub people: Vec<Person>,
    /// The password of each entry that can bind
    pub passwords: HashMap<String, String>,
}

impl Directory {
    pub fn with(mut self, person: Person, password: Option<&str>) -> Directory {
        if let Some(p) = password {
            self.passwords.insert(person.dn.to_ascii_lowercase(), p.into());
        }
        self.people.push(person);
        self
    }

    /// Whether `person` is in the filter's answer; the chain rule of Active Directory follows groups inside groups
    fn matches(&self, person: &Person, filter: &Filter) -> bool {
        let text = |v: &[u8]| String::from_utf8_lossy(v).to_ascii_lowercase();
        match filter {
            Filter::And(parts) => parts.iter().all(|f| self.matches(person, f)),
            Filter::Or(parts) => parts.iter().any(|f| self.matches(person, f)),
            Filter::Not(part) => !self.matches(person, part),
            Filter::Equal(attr, value) | Filter::Approx(attr, value) => person.values(attr).iter().any(|v| v.to_ascii_lowercase() == text(value)),
            Filter::GreaterOrEqual(attr, value) => person.values(attr).iter().any(|v| v.to_ascii_lowercase() >= text(value)),
            Filter::LessOrEqual(attr, value) => person.values(attr).iter().any(|v| v.to_ascii_lowercase() <= text(value)),
            Filter::Present(attr) => !person.values(attr).is_empty(),
            Filter::Substrings { attr, initial, any, last } => person.values(attr).iter().any(|v| {
                let v = v.to_ascii_lowercase();
                let mut at = 0;
                if let Some(i) = initial {
                    if !v.starts_with(&text(i)) {
                        return false;
                    }
                    at = i.len();
                }
                for a in any {
                    match v[at..].find(&text(a)) {
                        Some(p) => at += p + a.len(),
                        None => return false,
                    }
                }
                last.as_ref().is_none_or(|l| v[at..].ends_with(&text(l)))
            }),
            Filter::Extensible { rule, attr, value, .. } => {
                let (Some(attr), want) = (attr, text(value)) else { return false };
                if rule.as_deref() == Some("1.2.840.113556.1.4.1941") {
                    return self.in_chain(person, attr, &want, 0);
                }
                person.values(attr).iter().any(|v| v.to_ascii_lowercase() == want)
            }
        }
    }

    /// `person` has `attr` = `want`, directly or through the entries it names (a group inside a group)
    fn in_chain(&self, person: &Person, attr: &str, want: &str, depth: usize) -> bool {
        if depth > 10 {
            return false;
        }
        person.values(attr).iter().any(|v| {
            let v = v.to_ascii_lowercase();
            v == want || self.people.iter().find(|p| p.dn.to_ascii_lowercase() == v).is_some_and(|p| self.in_chain(p, attr, want, depth + 1))
        })
    }
}

#[derive(Clone, Default)]
pub struct Options {
    /// TLS from the first byte
    pub ldaps: bool,
    /// Agrees to StartTLS
    pub starttls: bool,
    /// How long before every answer
    pub delay: Duration,
    /// Takes the connection and says nothing
    pub silent: bool,
    /// Answers with bytes that aren't LDAP
    pub garbage: bool,
    /// Answers a search with a message that claims to be this long
    pub claim_length: Option<usize>,
    /// The most entries the server gives a page, or in one answer (0 = all)
    pub page_limit: usize,
    /// Says a result of this code at the end of every search instead of what was found
    pub result_code: Option<u32>,
    /// Sends a reference to another server among the entries
    pub referral: bool,
}

/// What the server was asked, in order
#[derive(Default)]
pub struct Log {
    pub binds: Mutex<Vec<(String, String)>>,
    pub searches: Mutex<Vec<(String, Filter)>>,
}

pub struct Server {
    pub port: u16,
    /// What the directory holds now: a test changes it while the server runs
    pub directory: Arc<Mutex<Directory>>,
    /// The authority a client has to trust for the TLS of this server (DER)
    pub root: Vec<u8>,
    pub log: Arc<Log>,
}

impl Server {
    pub fn config(&self) -> Config {
        Config { host: "127.0.0.1".into(), port: self.port, security: super::Security::None, roots: Vec::new(), timeout: Duration::from_millis(1500) }
    }

    pub fn secure_config(&self, security: super::Security) -> Config {
        Config { security, roots: vec![self.root.clone()], ..self.config() }
    }
}

fn certificate() -> (Vec<u8>, Arc<rustls::ServerConfig>) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.distinguished_name.push(DnType::CommonName, "Test authority");
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["127.0.0.1".into()]).unwrap().signed_by(&key, &issuer).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![leaf.der().clone()], rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()))
        .unwrap();
    (ca.der().to_vec(), Arc::new(config))
}

pub async fn serve(directory: Directory, options: Options) -> Server {
    let (root, tls) = certificate();
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Log::default());
    let shared = log.clone();
    let directory = Arc::new(Mutex::new(directory));
    let kept = directory.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let (directory, options, acceptor, log) = (directory.clone(), options.clone(), acceptor.clone(), shared.clone());
            tokio::spawn(async move {
                if options.silent {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    return;
                }
                if options.ldaps {
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    let _ = session(Box::new(tls), &directory, &options, &log, None).await;
                } else {
                    let _ = session(Box::new(tcp), &directory, &options, &log, Some(acceptor)).await;
                }
            });
        }
    });
    Server { port, directory: kept, root, log }
}

fn message(id: i64, op: Vec<u8>, controls: Option<Vec<u8>>) -> Vec<u8> {
    let mut parts = vec![ber::int(ber::INTEGER, id), op];
    parts.extend(controls);
    ber::constructed(ber::SEQUENCE, &parts)
}

fn result(tag: u8, code: u32, text: &str) -> Vec<u8> {
    ber::constructed(tag, &[ber::int(ber::ENUMERATED, i64::from(code)), ber::octets(ber::OCTET_STRING, b""), ber::octets(ber::OCTET_STRING, text.as_bytes())])
}

async fn read_message<S: AsyncRead + Unpin>(io: &mut S) -> Option<Vec<u8>> {
    let mut head = [0u8; 2];
    io.read_exact(&mut head).await.ok()?;
    let mut raw = head.to_vec();
    let len = if head[1] & 0x80 == 0 {
        usize::from(head[1])
    } else {
        let n = usize::from(head[1] & 0x7f);
        let mut bytes = [0u8; 4];
        io.read_exact(&mut bytes[..n]).await.ok()?;
        raw.extend_from_slice(&bytes[..n]);
        bytes[..n].iter().fold(0usize, |a, b| (a << 8) | usize::from(*b))
    };
    let start = raw.len();
    raw.resize(start + len, 0);
    io.read_exact(&mut raw[start..]).await.ok()?;
    Some(raw)
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// One connection. `upgrade`: StartTLS can still be agreed to.
async fn session(mut io: Box<dyn Io>, shared: &Mutex<Directory>, options: &Options, log: &Log, mut upgrade: Option<tokio_rustls::TlsAcceptor>) -> Option<()> {
    loop {
        let raw = read_message(&mut io).await?;
        // What the directory holds at this moment
        let directory = &shared.lock().clone();
        if options.delay > Duration::ZERO {
            tokio::time::sleep(options.delay).await;
        }
        if options.garbage {
            io.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await.ok()?;
            return None;
        }
        let (outer, _) = ber::read(&raw).ok()?;
        let parts = ber::items(outer.content).ok()?;
        let id = parts[0].int().ok()?;
        let op = parts[1];
        let controls = parts.get(2).map(|c| c.content).unwrap_or_default();
        match op.tag {
            t if t == APPLICATION | CONSTRUCTED => {
                let fields = ber::items(op.content).ok()?;
                let (dn, password) = (fields[1].text(), fields[2].text());
                log.binds.lock().push((dn.clone(), password.clone()));
                // A bind without a password is "anonymous": servers say it worked
                let ok = password.is_empty() || directory.passwords.get(&dn.to_ascii_lowercase()).is_some_and(|p| *p == password);
                let reply = if ok { result(APPLICATION | CONSTRUCTED | 1, 0, "") } else { result(APPLICATION | CONSTRUCTED | 1, 49, "invalid credentials") };
                io.write_all(&message(id, reply, None)).await.ok()?;
            }
            t if t == APPLICATION | 2 => return None,
            t if t == APPLICATION | CONSTRUCTED | 23 => {
                let Some(acceptor) = upgrade.clone().filter(|_| options.starttls) else {
                    io.write_all(&message(id, result(APPLICATION | CONSTRUCTED | 24, 2, "StartTLS isn't offered"), None)).await.ok()?;
                    continue;
                };
                io.write_all(&message(id, result(APPLICATION | CONSTRUCTED | 24, 0, ""), None)).await.ok()?;
                // The same connection goes on, secure
                let plain = io;
                io = Box::new(acceptor.accept(plain).await.ok()?);
                upgrade = None;
            }
            t if t == APPLICATION | CONSTRUCTED | 3 => {
                let fields = ber::items(op.content).ok()?;
                let base = fields[0].text();
                let size_limit = usize::try_from(fields[3].int().ok()?).unwrap_or(0);
                let filter = Filter::from_ber(fields[6]).ok()?;
                log.searches.lock().push((base.clone(), filter.clone()));
                if let Some(len) = options.claim_length {
                    // The start of a message that says it is `len` bytes long, and then nothing
                    let mut head = vec![ber::SEQUENCE, 0x84];
                    head.extend_from_slice(&(len as u32).to_be_bytes());
                    io.write_all(&head).await.ok()?;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    return None;
                }
                let wanted: Vec<String> = ber::items(fields[7].content).ok()?.iter().map(|a| a.text()).collect();
                let mut found: Vec<&Person> =
                    directory.people.iter().filter(|p| p.dn.to_ascii_lowercase().ends_with(&base.to_ascii_lowercase()) && directory.matches(p, &filter)).collect();
                // The page the client asked for (RFC 2696): the cookie is how far it got
                let (page, from) = paged(controls);
                let limit = [size_limit, options.page_limit, page.unwrap_or(0)].into_iter().filter(|n| *n > 0).min().unwrap_or(usize::MAX);
                found.drain(..from.min(found.len()));
                let truncated = found.len() > limit;
                found.truncate(limit);
                let sent = found.len();
                for p in found {
                    let attrs: Vec<Vec<u8>> = p
                        .attributes
                        .iter()
                        .filter(|(k, _)| wanted.is_empty() || wanted.iter().any(|w| w.eq_ignore_ascii_case(k)))
                        .map(|(k, v)| {
                            ber::constructed(
                                ber::SEQUENCE,
                                &[
                                    ber::octets(ber::OCTET_STRING, k.as_bytes()),
                                    ber::constructed(ber::SET, &v.iter().map(|s| ber::octets(ber::OCTET_STRING, s.as_bytes())).collect::<Vec<_>>()),
                                ],
                            )
                        })
                        .collect();
                    let entry =
                        ber::constructed(APPLICATION | CONSTRUCTED | 4, &[ber::octets(ber::OCTET_STRING, p.dn.as_bytes()), ber::constructed(ber::SEQUENCE, &attrs)]);
                    io.write_all(&message(id, entry, None)).await.ok()?;
                }
                if options.referral {
                    let reference = ber::constructed(APPLICATION | CONSTRUCTED | 19, &[ber::octets(ber::OCTET_STRING, b"ldap://elsewhere.example.com/dc=x")]);
                    io.write_all(&message(id, reference, None)).await.ok()?;
                }
                let (code, text) = match options.result_code {
                    Some(c) => (c, "refused"),
                    // A size limit the client asked for ends the search with its own code
                    None if truncated && page.is_none() => (4, "size limit exceeded"),
                    None => (0, ""),
                };
                let more = truncated && page.is_some();
                let cookie = if more { (from + sent).to_string().into_bytes() } else { Vec::new() };
                let control = page.map(|_| {
                    let value = ber::constructed(ber::SEQUENCE, &[ber::int(ber::INTEGER, 0), ber::octets(ber::OCTET_STRING, &cookie)]);
                    let control =
                        ber::constructed(ber::SEQUENCE, &[ber::octets(ber::OCTET_STRING, b"1.2.840.113556.1.4.319"), ber::octets(ber::OCTET_STRING, &value)]);
                    ber::constructed(CONTEXT | CONSTRUCTED, &[control])
                });
                io.write_all(&message(id, result(APPLICATION | CONSTRUCTED | 5, code, text), control)).await.ok()?;
            }
            _ => return None,
        }
    }
}

/// The page size and where it starts, from a search's controls (cookie: the number of entries already sent)
fn paged(controls: &[u8]) -> (Option<usize>, usize) {
    for control in ber::items(controls).unwrap_or_default() {
        let parts = ber::items(control.content).unwrap_or_default();
        if parts.first().map(|p| p.content) == Some(b"1.2.840.113556.1.4.319")
            && let Some(value) = parts.last().and_then(|v| ber::read(v.content).ok()).map(|(s, _)| ber::items(s.content).unwrap_or_default())
        {
            let size = value.first().and_then(|s| s.int().ok()).map(|n| n as usize);
            let from = value.get(1).and_then(|c| String::from_utf8_lossy(c.content).parse().ok()).unwrap_or(0);
            return (size, from);
        }
    }
    (None, 0)
}
