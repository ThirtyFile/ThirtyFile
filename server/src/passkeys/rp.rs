//! The relying party: the site passkeys belong to. Its id is the host name of the site's address (the "Site URL" setting,
//! never the `Host` header, which whoever sends the request chooses), and its origin that address. Passkeys need a name
//! (not an address of numbers) and a secure page: `https`, or `localhost` for development. Without them they are
//! not offered.

use super::webauthn::RelyingParty;
use crate::state::AppState;

/// The relying party of this site, when its address can carry passkeys
pub fn relying_party(st: &AppState) -> Option<RelyingParty> {
    from_url(&st.system.read().public_url)
}

pub fn from_url(url: &str) -> Option<RelyingParty> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return None;
    }
    // The address is host and port; a path, a query or a user name in it isn't an address of the site
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || authority.starts_with('[') {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p.parse::<u16>().ok().filter(|p| *p != 0)?)),
        None => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    // A host name: letters, digits, dots and dashes, with at least a letter somewhere (not an address of numbers)
    if host.is_empty()
        || !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        || host.starts_with('.')
        || host.ends_with('.')
        || host.parse::<std::net::Ipv4Addr>().is_ok()
    {
        return None;
    }
    if !host.chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let local = host == "localhost" || host.ends_with(".localhost");
    if scheme == "http" && !local {
        return None;
    }
    let default = if scheme == "https" { 443 } else { 80 };
    let origin = match port.filter(|p| *p != default) {
        Some(p) => format!("{scheme}://{host}:{p}"),
        None => format!("{scheme}://{host}"),
    };
    Some(RelyingParty { id: host, origin })
}

#[cfg(test)]
mod tests;
