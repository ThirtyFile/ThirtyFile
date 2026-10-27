use super::*;

fn id_and_origin(url: &str) -> Option<(String, String)> {
    from_url(url).map(|rp| (rp.id, rp.origin))
}

#[test]
fn the_site_address_gives_the_id_and_the_origin() {
    let some = |id: &str, origin: &str| Some((id.to_string(), origin.to_string()));
    assert_eq!(id_and_origin("https://Files.Example.com"), some("files.example.com", "https://files.example.com"));
    assert_eq!(id_and_origin("https://files.example.com/"), some("files.example.com", "https://files.example.com"));
    assert_eq!(id_and_origin("https://files.example.com:8443/drive"), some("files.example.com", "https://files.example.com:8443"));
    // The default port is not part of an origin
    assert_eq!(id_and_origin("https://files.example.com:443"), some("files.example.com", "https://files.example.com"));
    assert_eq!(id_and_origin("  https://files.example.com  "), some("files.example.com", "https://files.example.com"));
}

#[test]
fn localhost_may_use_http_for_development() {
    assert_eq!(id_and_origin("http://localhost:5173"), Some(("localhost".into(), "http://localhost:5173".into())));
    assert_eq!(id_and_origin("http://app.localhost"), Some(("app.localhost".into(), "http://app.localhost".into())));
}

#[test]
fn addresses_passkeys_cannot_belong_to_are_refused() {
    for url in [
        "",
        "files.example.com",
        // Not secure
        "http://files.example.com",
        // A number is not a name a passkey can be made for
        "https://192.0.2.10",
        "https://192.0.2.10:8443",
        "https://[::1]",
        "https://1234",
        // Not an address of a site
        "ftp://files.example.com",
        "https://user@files.example.com",
        "https://",
        "https://:443",
        "https://files.example.com:0",
        "https://files.example.com:99999",
        "https://files_example.com",
        "https://.example.com",
        "https://example.com.",
    ] {
        assert_eq!(id_and_origin(url), None, "{url}");
    }
}
