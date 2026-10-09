//! Properties: reading PROPFIND and PROPPATCH requests and writing the answers

use super::*;

/// Which properties a PROPFIND asks for
#[derive(Debug, PartialEq, Clone)]
pub(super) enum Want {
    All,
    Names,
    /// (namespace, local name)
    Props(Vec<(String, String)>),
}

impl Want {
    pub(super) fn asks_for(&self, name: &str) -> bool {
        matches!(self, Want::Props(list) if list.iter().any(|(ns, local)| ns == DAV_NS && local == name))
    }
}

/// What the XML body of a PROPFIND, PROPPATCH or LOCK says
#[derive(Default, Debug)]
pub(super) struct Parsed {
    pub(super) allprop: bool,
    pub(super) propname: bool,
    /// A `prop` element was there (PROPFIND for named properties)
    pub(super) prop: bool,
    /// Children of every `prop` element: (namespace, local name)
    pub(super) props: Vec<(String, String)>,
    /// LOCK: the text of `owner`
    pub(super) owner: String,
    /// LOCK: a shared lock was asked for
    pub(super) shared: bool,
}

pub(super) fn parse(xml: &[u8]) -> AppResult<Parsed> {
    let bad = || AppError::bad_request("The XML in the request isn't valid");
    let mut out = Parsed::default();
    if xml.iter().all(u8::is_ascii_whitespace) {
        return Ok(out);
    }
    let mut r = NsReader::from_reader(xml);
    r.config_mut().trim_text(true);
    // The open elements: (namespace, local name)
    let mut open: Vec<(String, String)> = Vec::new();
    loop {
        let (ns, event) = r.read_resolved_event().map_err(|_| bad())?;
        let ns = match ns {
            ResolveResult::Bound(n) => n.as_ref().to_owned(),
            _ => String::new(),
        };
        let (start, empty) = match &event {
            Event::Start(e) => (Some(e), false),
            Event::Empty(e) => (Some(e), true),
            _ => (None, false),
        };
        if let Some(e) = start {
            let local = e.local_name().as_ref().to_owned();
            let in_prop = open.last().is_some_and(|(n, l)| n == DAV_NS && l == "prop");
            let in_owner = open.iter().any(|(n, l)| n == DAV_NS && l == "owner");
            if in_prop {
                out.props.push((ns.clone(), local.clone()));
            } else if ns == DAV_NS && !in_owner {
                match local.as_str() {
                    "allprop" => out.allprop = true,
                    "propname" => out.propname = true,
                    "prop" => out.prop = true,
                    "shared" if open.iter().any(|(n, l)| n == DAV_NS && l == "lockscope") => out.shared = true,
                    _ => {}
                }
            }
            if !empty {
                open.push((ns, local));
            }
            continue;
        }
        match event {
            Event::End(_) => {
                open.pop();
            }
            Event::Text(t) if open.iter().any(|(n, l)| n == DAV_NS && l == "owner") => {
                out.owner.push_str(&t.xml10_content());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

impl Parsed {
    pub(super) fn want(self) -> Want {
        if self.propname {
            Want::Names
        } else if self.prop && !self.allprop {
            Want::Props(self.props)
        } else {
            Want::All
        }
    }
}

pub(super) async fn read_xml(body: Body) -> AppResult<Parsed> {
    let bytes = axum::body::to_bytes(body, MAX_XML).await.map_err(|_| AppError::new(StatusCode::PAYLOAD_TOO_LARGE, "The request is too large"))?;
    parse(&bytes)
}

/// Seconds since 1970 in the format of HTTP headers (RFC 1123): "Sun, 06 Nov 1994 08:49:37 GMT"
pub(super) fn http_date(t: i64) -> String {
    httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(crate::util::file_time(t) as u64))
}

/// Seconds since 1970 as RFC 3339 in UTC: "1994-11-06T08:49:37Z"
pub(super) fn rfc3339(t: i64) -> String {
    let t = crate::util::file_time(t);
    let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
    // Days to a civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

pub(super) const SUPPORTED_LOCK: &str = "<D:lockentry><D:lockscope><D:exclusive/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockentry>\
<D:lockentry><D:lockscope><D:shared/></D:lockscope><D:locktype><D:write/></D:locktype></D:lockentry>";

/// Properties (DAV: namespace) of an item, as XML content; `quota`: (used, available) of the space
pub(super) fn node_props(node: &Node, display: &str, quota: Option<(i64, i64)>) -> Vec<(&'static str, String)> {
    let mut p = vec![
        ("displayname", escape(display).into_owned()),
        ("resourcetype", if node.is_folder() { "<D:collection/>".into() } else { String::new() }),
        ("getlastmodified", http_date(node.updated_at)),
        ("creationdate", rfc3339(node.created_at)),
    ];
    if node.is_folder() {
        p.push(("getetag", format!("\"{}-{}\"", node.id, node.updated_at)));
    } else {
        p.push(("getcontentlength", node.size.to_string()));
        let mime = if node.mime.is_empty() { "application/octet-stream" } else { &node.mime };
        p.push(("getcontenttype", escape(mime).into_owned()));
        // The same tag GET gives stored content; files of folder spaces get one from their index entry
        let tag = match &node.blob_hash {
            Some(hash) if !node.in_folder_space() => hash.clone(),
            _ => format!("{}-{}-{}", node.id, node.updated_at, node.size),
        };
        p.push(("getetag", format!("\"{}\"", escape(&tag))));
    }
    p.push(("supportedlock", SUPPORTED_LOCK.into()));
    p.push(("lockdiscovery", String::new()));
    if let Some((used, available)) = quota {
        p.push(("quota-used-bytes", used.to_string()));
        p.push(("quota-available-bytes", available.to_string()));
    }
    p
}

/// Properties of `/dav/` and "Shared with me", which aren't items
pub(super) fn folder_props(display: &str) -> Vec<(&'static str, String)> {
    vec![
        ("displayname", escape(display).into_owned()),
        ("resourcetype", "<D:collection/>".into()),
        ("getlastmodified", http_date(now())),
        ("supportedlock", SUPPORTED_LOCK.into()),
        ("lockdiscovery", String::new()),
    ]
}

/// A 207 Multi-Status response being written
pub(super) struct Multistatus {
    pub(super) xml: String,
}

impl Multistatus {
    pub(super) fn new() -> Self {
        Multistatus { xml: r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">"#.into() }
    }

    pub(super) fn propstat(&mut self, props: &str, status: &str) {
        self.xml.push_str(&format!("<D:propstat><D:prop>{props}</D:prop><D:status>HTTP/1.1 {status}</D:status></D:propstat>"));
    }

    /// One item with the properties asked for; the ones it doesn't have are reported as not found
    pub(super) fn add(&mut self, href: &str, want: &Want, props: Vec<(&'static str, String)>) {
        let element = |name: &str, value: &str| if value.is_empty() { format!("<D:{name}/>") } else { format!("<D:{name}>{value}</D:{name}>") };
        let mut found = String::new();
        let mut missing = String::new();
        match want {
            // Quotas are only reported when asked for by name (RFC 4331)
            Want::All => props.iter().filter(|(n, _)| !n.starts_with("quota-")).for_each(|(n, v)| found.push_str(&element(n, v))),
            Want::Names => props.iter().for_each(|(n, _)| found.push_str(&format!("<D:{n}/>"))),
            Want::Props(list) => {
                for (ns, local) in list {
                    match props.iter().find(|(n, _)| ns == DAV_NS && n == local) {
                        Some((n, v)) => found.push_str(&element(n, v)),
                        None => missing.push_str(&foreign(ns, local)),
                    }
                }
            }
        }
        self.xml.push_str(&format!("<D:response><D:href>{}</D:href>", escape(href)));
        if !found.is_empty() || missing.is_empty() {
            self.propstat(&found, "200 OK");
        }
        if !missing.is_empty() {
            self.propstat(&missing, "404 Not Found");
        }
        self.xml.push_str("</D:response>");
    }

    /// The start of a multistatus whose responses follow as a stream (`Multistatus::part`): a large folder isn't held
    /// in memory as one answer
    pub(super) fn part() -> Self {
        Multistatus { xml: String::new() }
    }

    pub(super) fn finish(mut self) -> Response {
        self.xml.push_str("</D:multistatus>");
        (StatusCode::MULTI_STATUS, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], self.xml).into_response()
    }
}

/// An empty element named as the request named it
pub(super) fn foreign(ns: &str, local: &str) -> String {
    match ns {
        DAV_NS => format!("<D:{local}/>"),
        "" => format!("<{local} xmlns=\"\"/>"),
        _ => format!("<R:{local} xmlns:R=\"{}\"/>", escape(ns)),
    }
}

/// (used, available) bytes of a space with a quota, remembered per space for one response
pub(super) async fn quota(conn: &mut SqliteConnection, cache: &mut HashMap<String, Option<(i64, i64)>>, drive_id: &str) -> AppResult<Option<(i64, i64)>> {
    if let Some(q) = cache.get(drive_id) {
        return Ok(*q);
    }
    let q = match tree::get_drive(conn, drive_id).await? {
        Some(d) => {
            let limit = tree::drive_quota(conn, &d).await?;
            (limit > 0).then(|| (d.used_bytes, (limit - d.used_bytes).max(0)))
        }
        None => None,
    };
    cache.insert(drive_id.to_string(), q);
    Ok(q)
}
