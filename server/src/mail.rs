//! Sending email through the organization's own email server (SMTP), for notifications (notify.rs).
//!
//! - Off until an administrator enters a server under Control panel › Email; the password is stored encrypted (secrets.rs)
//! - STARTTLS (usually port 587), TLS from the start (465) or no encryption (a relay on the local network, port 25);
//!   certificates are verified against the operating system's, which can be skipped for self-signed ones
//! - A small client of its own: one message per connection, sign-in with AUTH PLAIN or LOGIN, the text sent as UTF-8
//!   in base64, so no line can be mistaken for the end of the message

use std::time::Duration;

use axum::{Json, extract::State, http::HeaderMap};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpStream,
};

use crate::{
    auth::Admin,
    db::{get_setting, set_setting},
    error::{AppError, AppResult},
    logs,
    state::AppState,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A whole message, from connecting to the server's answer to the text
const SEND_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest server reply read (a list of extensions is a few hundred bytes)
const MAX_REPLY: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    #[default]
    Starttls,
    Tls,
    None,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SmtpSettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// Blank = the server doesn't ask to sign in (a relay that trusts this server's address)
    pub username: String,
    pub password: String,
    /// The sender's address; the site name is shown as the sender's name
    pub from: String,
    /// Accept any certificate (self-signed ones on a local server)
    pub insecure: bool,
}

impl SmtpSettings {
    pub fn ready(&self) -> bool {
        self.enabled && !self.host.is_empty() && !self.from.is_empty()
    }
}

pub async fn load(db: &sqlx::SqlitePool) -> SmtpSettings {
    let mut s: SmtpSettings = match get_setting(db, "smtp").await {
        Ok(Some(v)) => serde_json::from_str(&v).unwrap_or_else(|e| {
            tracing::error!("The email server settings can't be read and are ignored (saving them again replaces them): {e}");
            SmtpSettings::default()
        }),
        _ => SmtpSettings::default(),
    };
    match crate::secrets::open("smtp", &s.password) {
        Ok(plain) => s.password = plain,
        Err(e) => {
            tracing::error!("The email server password can't be read ({e}); emails aren't sent until it is entered again");
            s.password.clear();
            s.enabled = false;
        }
    }
    s
}

pub async fn store(conn: &mut sqlx::SqliteConnection, settings: &SmtpSettings) -> Result<(), sqlx::Error> {
    let sealed = SmtpSettings { password: crate::secrets::seal("smtp", &settings.password), ..settings.clone() };
    set_setting(conn, "smtp", &serde_json::to_string(&sealed).unwrap()).await
}

/// Whether `addr` is a plain email address (name@domain), with nothing that could end a header or a command
pub fn valid_address(addr: &str) -> bool {
    let Some((local, domain)) = addr.rsplit_once('@') else { return false };
    addr.len() <= 254
        && !local.is_empty()
        && !domain.is_empty()
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !addr.chars().any(|c| c.is_control() || c.is_whitespace() || matches!(c, '<' | '>' | '"' | ',' | ';' | '(' | ')' | '[' | ']' | '\\'))
        && !local.contains('@')
}

// ───────────── Control panel › Email ─────────────

fn admin_view(s: &SmtpSettings) -> Value {
    json!({
        "enabled": s.enabled,
        "host": s.host,
        "port": s.port,
        "security": s.security,
        "username": s.username,
        "has_password": !s.password.is_empty(),
        "from": s.from,
        "insecure": s.insecure,
    })
}

pub async fn get_settings(State(st): State<AppState>, _: Admin) -> Json<Value> {
    Json(admin_view(&load(&st.db).await))
}

#[derive(Deserialize)]
pub struct SettingsReq {
    enabled: bool,
    #[serde(default)]
    host: String,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    security: Security,
    #[serde(default)]
    username: String,
    /// Blank keeps the saved password
    #[serde(default)]
    password: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    insecure: bool,
}

/// The settings asked for, checked; a blank password keeps the saved one (and there is none without a username), but
/// only for the same server and account: otherwise it would be sent to wherever the new settings point, and anyone
/// able to change them could collect it (as for storage locations, locations.rs)
fn settings_from(req: SettingsReq, saved: &SmtpSettings) -> AppResult<SmtpSettings> {
    let host = req.host.trim().to_string();
    let from = req.from.trim().to_string();
    let username = req.username.trim().to_string();
    let valid_host = host.len() <= 253 && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '[' | ']'));
    if !valid_host {
        return Err(AppError::bad_request("Enter the email server's name or IP address, e.g. smtp.example.com"));
    }
    if !from.is_empty() && !valid_address(&from) {
        return Err(AppError::bad_request("The sender address isn't a valid email address"));
    }
    if req.enabled && (host.is_empty() || from.is_empty()) {
        return Err(AppError::bad_request("Enter the email server and the sender address to send emails"));
    }
    if username.chars().any(char::is_control) || req.password.chars().any(|c| c == '\r' || c == '\n') {
        return Err(AppError::bad_request("The username or password contains characters that can't be sent"));
    }
    let default_port = match req.security {
        Security::Starttls => 587,
        Security::Tls => 465,
        Security::None => 25,
    };
    let port = req.port.filter(|p| *p > 0).unwrap_or(default_port);
    let same_target = host.eq_ignore_ascii_case(&saved.host)
        && port == saved.port
        && req.security == saved.security
        && req.insecure == saved.insecure
        && username == saved.username;
    let password = match (username.is_empty(), req.password.is_empty()) {
        (true, _) => String::new(),
        (false, true) if saved.password.is_empty() => String::new(),
        (false, true) if same_target => saved.password.clone(),
        (false, true) => return Err(AppError::bad_request("Enter the password again: the email server or account changed")),
        (false, false) => req.password,
    };
    Ok(SmtpSettings { enabled: req.enabled, host, port, security: req.security, username, password, from, insecure: req.insecure })
}

pub async fn update_settings(State(st): State<AppState>, Admin(user): Admin, Json(req): Json<SettingsReq>) -> AppResult<Json<Value>> {
    let saved = load(&st.db).await;
    let s = settings_from(req, &saved)?;
    let _w = st.write_lock.lock().await;
    let mut tx = crate::db::begin_write(&st.db).await?;
    store(&mut tx, &s).await?;
    let detail = if s.enabled { format!("Email notifications are sent through {}:{}", s.host, s.port) } else { "Email notifications are off".to_string() };
    logs::record_activity(&mut tx, &user, None, "settings", &detail).await?;
    tx.commit().await?;
    Ok(Json(admin_view(&s)))
}

#[derive(Deserialize)]
pub struct TestReq {
    #[serde(flatten)]
    settings: SettingsReq,
    to: String,
}

/// Sends a test message with the settings on the page (saved or not), and says what went wrong
pub async fn test(State(st): State<AppState>, Admin(_): Admin, headers: HeaderMap, Json(req): Json<TestReq>) -> AppResult<Json<Value>> {
    let to = req.to.trim().to_string();
    if !valid_address(&to) {
        return Err(AppError::bad_request("Enter a valid email address to send the test to"));
    }
    let saved = load(&st.db).await;
    let s = settings_from(SettingsReq { enabled: true, ..req.settings }, &saved)?;
    let site = st.branding.read().unwrap().site_name.clone();
    // In the language of the page it was sent from
    let zh = crate::auth::get_cookie(&headers, "tf_lang") == Some("zh-TW");
    let (subject, body) = crate::notify::test_message(zh, &site);
    send(&s, &site, &Message { to: &to, subject: &subject, body: &body })
        .await
        .map_err(|e| AppError::bad_request(format!("The test email couldn't be sent: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

// ───────────── Sending ─────────────

pub struct Message<'a> {
    pub to: &'a str,
    pub subject: &'a str,
    /// Plain text; lines end with \n
    pub body: &'a str,
}

/// Sends one message. The error is a short English reason, for the log and the test button.
pub async fn send(cfg: &SmtpSettings, sender_name: &str, msg: &Message<'_>) -> Result<(), String> {
    match tokio::time::timeout(SEND_TIMEOUT, send_inner(cfg, sender_name, msg)).await {
        Ok(r) => r,
        Err(_) => Err("the email server didn't answer in time".into()),
    }
}

async fn send_inner(cfg: &SmtpSettings, sender_name: &str, msg: &Message<'_>) -> Result<(), String> {
    let addr = format!("{}:{}", cfg.host.trim_start_matches('[').trim_end_matches(']'), cfg.port);
    let tcp = match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&addr)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(format!("can't connect to {}:{} ({e})", cfg.host, cfg.port)),
        Err(_) => return Err(format!("can't connect to {}:{} (no answer)", cfg.host, cfg.port)),
    };
    let data = message_text(cfg, sender_name, msg);
    match cfg.security {
        Security::None => {
            let mut s = Session::new(tcp);
            s.expect(&[220]).await?;
            let caps = s.ehlo().await?;
            s.deliver(cfg, &caps, msg.to, &data).await
        }
        Security::Tls => {
            let mut s = Session::new(tls(cfg, tcp).await?);
            s.expect(&[220]).await?;
            let caps = s.ehlo().await?;
            s.deliver(cfg, &caps, msg.to, &data).await
        }
        Security::Starttls => {
            let mut s = Session::new(tcp);
            s.expect(&[220]).await?;
            let caps = s.ehlo().await?;
            if !caps.iter().any(|c| c.eq_ignore_ascii_case("STARTTLS")) {
                return Err("the email server doesn't offer STARTTLS; choose another encryption setting".into());
            }
            s.command("STARTTLS", &[220]).await?;
            let mut s = Session::new(tls(cfg, s.into_inner()).await?);
            let caps = s.ehlo().await?;
            s.deliver(cfg, &caps, msg.to, &data).await
        }
    }
}

async fn tls(cfg: &SmtpSettings, tcp: TcpStream) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    let config = crate::ftp::client_tls(cfg.insecure).map_err(|e| e.to_string())?;
    let host = cfg.host.trim_start_matches('[').trim_end_matches(']').to_string();
    let name = rustls::pki_types::ServerName::try_from(host).map_err(|_| "the email server's name isn't valid".to_string())?;
    tokio_rustls::TlsConnector::from(config).connect(name, tcp).await.map_err(|e| format!("secure connection failed ({e})"))
}

/// One conversation with the server
struct Session<S> {
    io: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Session<S> {
    fn new(io: S) -> Self {
        Self { io: BufReader::new(io) }
    }

    fn into_inner(self) -> S {
        self.io.into_inner()
    }

    /// Reads a reply ("250-first line" … "250 last line"): the code and the text of each line
    async fn reply(&mut self) -> Result<(u16, Vec<String>), String> {
        let mut lines = Vec::new();
        let mut read = 0u64;
        loop {
            let mut line = String::new();
            let n = (&mut self.io).take(MAX_REPLY - read).read_line(&mut line).await.map_err(|e| format!("connection lost ({e})"))?;
            if n == 0 {
                return Err("the email server closed the connection".into());
            }
            read += n as u64;
            if read >= MAX_REPLY {
                return Err("the email server's reply is too long".into());
            }
            let line = line.trim_end_matches(['\r', '\n']);
            let code = line.get(..3).and_then(|c| c.parse::<u16>().ok()).ok_or_else(|| format!("unexpected reply: {}", clip(line)))?;
            lines.push(line.get(4..).unwrap_or_default().to_string());
            if line.as_bytes().get(3) != Some(&b'-') {
                return Ok((code, lines));
            }
        }
    }

    async fn expect(&mut self, ok: &[u16]) -> Result<Vec<String>, String> {
        let (code, lines) = self.reply().await?;
        if !ok.contains(&code) {
            return Err(format!("the email server answered {code} {}", clip(&lines.join(" "))));
        }
        Ok(lines)
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), String> {
        let io = self.io.get_mut();
        io.write_all(data).await.map_err(|e| format!("connection lost ({e})"))?;
        io.flush().await.map_err(|e| format!("connection lost ({e})"))
    }

    async fn command(&mut self, cmd: &str, ok: &[u16]) -> Result<Vec<String>, String> {
        self.write(format!("{cmd}\r\n").as_bytes()).await?;
        self.expect(ok).await
    }

    /// Greets the server; returns the extensions it offers (e.g. "STARTTLS", "AUTH PLAIN LOGIN")
    async fn ehlo(&mut self) -> Result<Vec<String>, String> {
        let lines = self.command("EHLO thirtyfile", &[250]).await?;
        Ok(lines.into_iter().skip(1).collect())
    }

    async fn deliver(&mut self, cfg: &SmtpSettings, caps: &[String], to: &str, data: &str) -> Result<(), String> {
        if !cfg.username.is_empty() {
            let methods: Vec<String> = caps
                .iter()
                .filter_map(|c| c.strip_prefix("AUTH ").or_else(|| c.strip_prefix("auth ")))
                .flat_map(|m| m.split_whitespace().map(str::to_ascii_uppercase))
                .collect();
            let failed = |e: String| if e.contains("answered 535") { "the email server didn't accept the username or password".to_string() } else { e };
            if methods.iter().any(|m| m == "LOGIN") && !methods.iter().any(|m| m == "PLAIN") {
                self.command("AUTH LOGIN", &[334]).await?;
                self.command(&STANDARD.encode(&cfg.username), &[334]).await?;
                self.command(&STANDARD.encode(&cfg.password), &[235]).await.map_err(failed)?;
            } else {
                let plain = STANDARD.encode(format!("\0{}\0{}", cfg.username, cfg.password));
                self.command(&format!("AUTH PLAIN {plain}"), &[235]).await.map_err(failed)?;
            }
        }
        self.command(&format!("MAIL FROM:<{}>", cfg.from), &[250]).await?;
        self.command(&format!("RCPT TO:<{to}>"), &[250, 251]).await?;
        self.command("DATA", &[354]).await?;
        self.write(data.as_bytes()).await?;
        self.write(b".\r\n").await?;
        self.expect(&[250]).await?;
        // The message is accepted; how the server says goodbye doesn't matter
        let _ = self.command("QUIT", &[221]).await;
        Ok(())
    }
}

/// A reply shortened for an error message
fn clip(s: &str) -> String {
    s.chars().take(200).collect()
}

/// A header value: as is when it is plain ASCII, otherwise as UTF-8 encoded words (RFC 2047), folded so no line is too long
pub fn header_text(text: &str) -> String {
    let text: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if text.is_ascii() && !text.contains("=?") && text.len() <= 900 {
        return text;
    }
    // At most 45 bytes per word (60 characters of base64), cut between characters
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in text.chars() {
        if chunk.len() + c.len_utf8() > 45 {
            words.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(&chunk)));
            chunk.clear();
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        words.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(&chunk)));
    }
    words.join("\r\n ")
}

/// The whole message as sent after DATA (without the final "."): headers, then the text in base64
fn message_text(cfg: &SmtpSettings, sender_name: &str, msg: &Message<'_>) -> String {
    let domain = cfg.from.rsplit_once('@').map_or("localhost", |(_, d)| d);
    // Quoted when it is plain text, so a name with a comma or a dot stays one name
    let name = header_text(sender_name.trim());
    let name = if name.starts_with("=?") { name } else { format!("\"{}\"", name.replace(['"', '\\'], "")) };
    let body = STANDARD.encode(msg.body.replace("\r\n", "\n").replace('\n', "\r\n"));
    let mut out = format!(
        "From: {name} <{}>\r\nTo: <{}>\r\nSubject: {}\r\nDate: {}\r\nMessage-ID: <{}@{domain}>\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\nAuto-Submitted: auto-generated\r\n\r\n",
        cfg.from,
        msg.to,
        header_text(msg.subject),
        httpdate::fmt_http_date(std::time::SystemTime::now()),
        crate::util::new_id(),
    );
    for line in body.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A pretend email server on a local port: answers every command, takes a sign-in, and hands over each message
    /// it receives (the lines after DATA, decoded)
    pub async fn fake_server(auth: bool) -> (u16, tokio::sync::mpsc::UnboundedReceiver<(String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut io = BufReader::new(sock);
                    let mut rcpt = String::new();
                    let mut signed_in = !auth;
                    io.get_mut().write_all(b"220 fake ready\r\n").await.unwrap();
                    loop {
                        let mut line = String::new();
                        if io.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        let cmd = line.trim_end().to_string();
                        let answer: &[u8] = if cmd.starts_with("EHLO") {
                            b"250-fake\r\n250-AUTH PLAIN\r\n250 8BITMIME\r\n"
                        } else if let Some(plain) = cmd.strip_prefix("AUTH PLAIN ") {
                            signed_in = STANDARD.decode(plain).unwrap() == b"\0mailer\0secret";
                            if signed_in { b"235 ok\r\n" } else { b"535 no\r\n" }
                        } else if cmd.starts_with("MAIL FROM") {
                            if signed_in { b"250 ok\r\n" } else { b"530 sign in first\r\n" }
                        } else if let Some(to) = cmd.strip_prefix("RCPT TO:") {
                            rcpt = to.trim_matches(['<', '>']).to_string();
                            b"250 ok\r\n"
                        } else if cmd == "DATA" {
                            io.get_mut().write_all(b"354 go\r\n").await.unwrap();
                            let mut text = String::new();
                            loop {
                                let mut l = String::new();
                                io.read_line(&mut l).await.unwrap();
                                if l == ".\r\n" {
                                    break;
                                }
                                text.push_str(&l);
                            }
                            let _ = tx.send((rcpt.clone(), decode(&text)));
                            b"250 queued\r\n"
                        } else if cmd == "QUIT" {
                            let _ = io.get_mut().write_all(b"221 bye\r\n").await;
                            return;
                        } else {
                            b"500 what\r\n"
                        };
                        io.get_mut().write_all(answer).await.unwrap();
                    }
                });
            }
        });
        (port, rx)
    }

    /// Headers as they are, followed by the body decoded from base64
    fn decode(text: &str) -> String {
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        let body = STANDARD.decode(body.replace("\r\n", "")).unwrap();
        format!("{head}\r\n\r\n{}", String::from_utf8(body).unwrap())
    }

    pub fn settings(port: u16) -> SmtpSettings {
        SmtpSettings {
            enabled: true,
            host: "127.0.0.1".into(),
            port,
            security: Security::None,
            username: "mailer".into(),
            password: "secret".into(),
            from: "drive@example.com".into(),
            insecure: false,
        }
    }

    #[tokio::test]
    async fn sends_a_message_and_reports_a_refused_sign_in() {
        let (port, mut rx) = fake_server(true).await;
        let msg = Message { to: "amy@example.com", subject: "分享 “Plans”", body: "Line one\n.\nLine three" };
        send(&settings(port), "Drive, Inc.", &msg).await.unwrap();
        let (to, text) = rx.recv().await.unwrap();
        assert_eq!(to, "amy@example.com");
        assert!(text.contains("From: \"Drive, Inc.\" <drive@example.com>"), "{text}");
        assert!(text.contains("Subject: =?UTF-8?B?"), "a Chinese subject is encoded: {text}");
        // A line holding only "." is part of the text, not the end of the message
        assert!(text.ends_with("Line one\r\n.\r\nLine three"), "{text}");

        let wrong = SmtpSettings { password: "nope".into(), ..settings(port) };
        let err = send(&wrong, "Drive", &msg).await.unwrap_err();
        assert!(err.contains("username or password"), "{err}");
        let closed = SmtpSettings { port: 1, ..settings(port) };
        assert!(send(&closed, "Drive", &msg).await.unwrap_err().contains("can't connect"));
    }

    #[test]
    fn addresses_and_headers() {
        assert!(valid_address("amy@example.com"));
        assert!(valid_address("a.b+c@mail.example.co.uk"));
        for bad in ["amy", "@example.com", "amy@", "amy@.com", "a b@example.com", "amy@example.com\r\nBcc: x@y.z", "<amy@example.com>", "a@b@c"] {
            assert!(!valid_address(bad), "{bad}");
        }
        assert_eq!(header_text("Plain subject"), "Plain subject");
        assert_eq!(header_text("Line\r\nBcc: x"), "Line  Bcc: x");
        let long = header_text(&"空間快滿了".repeat(10));
        assert!(long.lines().all(|l| l.len() <= 76), "{long}");
        let decoded: String = long
            .split("\r\n ")
            .map(|w| String::from_utf8(STANDARD.decode(w.trim_start_matches("=?UTF-8?B?").trim_end_matches("?=")).unwrap()).unwrap())
            .collect();
        assert_eq!(decoded, "空間快滿了".repeat(10));
    }

    #[test]
    fn settings_are_checked_and_keep_the_password() {
        let saved = settings(25);
        let req = |v: Value| serde_json::from_value::<SettingsReq>(v).unwrap();
        // The saved password is kept for the same server and account…
        let s = settings_from(req(json!({ "enabled": true, "host": " 127.0.0.1 ", "port": 25, "security": "none", "username": "mailer", "from": "drive@example.com" })), &saved).unwrap();
        assert_eq!((s.host.as_str(), s.port, s.password.as_str()), ("127.0.0.1", 25, "secret"));
        // …and never sent anywhere else: another server, port, account, encryption or certificate check asks for it again
        for changed in [
            json!({ "host": "smtp.example.com", "port": 25, "username": "mailer" }),
            json!({ "host": "127.0.0.1", "port": 2525, "username": "mailer" }),
            json!({ "host": "127.0.0.1", "port": 25, "username": "someone" }),
            json!({ "host": "127.0.0.1", "port": 25, "username": "mailer", "security": "tls" }),
            json!({ "host": "127.0.0.1", "port": 25, "username": "mailer", "insecure": true }),
        ] {
            let mut v = changed.clone();
            if v.get("security").is_none() {
                v["security"] = json!("none");
            }
            v["enabled"] = json!(true);
            v["from"] = json!("drive@example.com");
            assert!(settings_from(req(v.clone()), &saved).is_err(), "{changed}");
            v["password"] = json!("typed again");
            assert_eq!(settings_from(req(v), &saved).unwrap().password, "typed again");
        }
        let s = settings_from(req(json!({ "enabled": true, "host": " smtp.example.com ", "security": "tls", "username": "mailer", "password": "new", "from": "drive@example.com" })), &saved).unwrap();
        assert_eq!((s.host.as_str(), s.port, s.password.as_str()), ("smtp.example.com", 465, "new"));
        let s = settings_from(req(json!({ "enabled": false, "host": "smtp.example.com", "from": "drive@example.com" })), &saved).unwrap();
        assert!(s.password.is_empty(), "no username, no password");
        assert!(settings_from(req(json!({ "enabled": true, "host": "", "from": "drive@example.com" })), &saved).is_err());
        assert!(settings_from(req(json!({ "enabled": true, "host": "smtp.example.com", "from": "not an address" })), &saved).is_err());
        assert!(settings_from(req(json!({ "enabled": true, "host": "smtp.example.com/x", "from": "drive@example.com" })), &saved).is_err());
    }
}
