//! Turning what people type or click into a URL, and deciding where a link opens.

use url::{Host, Url};

/// Where links from the terminal, the Markdown preview and the timeline open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenLinksIn {
    /// Local addresses in Solo's browser, everything else in the system browser.
    #[default]
    LocalInSolo,
    AllInSolo,
    AllInSystem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    Solo,
    System,
}

/// Where `url` opens under `setting`. `alternate` (Ctrl+Shift+click) picks the
/// other one. Only http(s) and local file URLs can open in Solo.
pub fn link_target(url: &str, setting: OpenLinksIn, alternate: bool) -> LinkTarget {
    let parsed = Url::parse(url).ok();
    let can_open_in_solo = parsed.as_ref().is_some_and(is_allowed_in_solo);
    if !can_open_in_solo {
        return LinkTarget::System;
    }
    let preferred = match setting {
        OpenLinksIn::AllInSolo => LinkTarget::Solo,
        OpenLinksIn::AllInSystem => LinkTarget::System,
        OpenLinksIn::LocalInSolo => {
            if parsed.as_ref().is_some_and(is_local) {
                LinkTarget::Solo
            } else {
                LinkTarget::System
            }
        }
    };
    match (preferred, alternate) {
        (target, false) => target,
        (LinkTarget::Solo, true) => LinkTarget::System,
        (LinkTarget::System, true) => LinkTarget::Solo,
    }
}

/// http, https, and HTML files on this computer.
pub fn is_allowed_in_solo(url: &Url) -> bool {
    match url.scheme() {
        "http" | "https" => url.host().is_some(),
        "file" => url
            .to_file_path()
            .ok()
            .and_then(|path| {
                path.extension()
                    .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            })
            .is_some_and(|extension| extension == "html" || extension == "htm"),
        "about" => url.as_str() == "about:blank",
        _ => false,
    }
}

/// localhost, 127.0.0.1 (all of 127/8), [::1], *.localhost, and local files.
pub fn is_local(url: &Url) -> bool {
    if url.scheme() == "file" {
        return true;
    }
    match url.host() {
        Some(Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

/// What the address bar does with what was typed: `5173` and `localhost:5173`
/// become `http://localhost:5173/`; a bare host gets `https://` unless it is
/// local; paths to HTML files become file URLs. Returns `None` for text that
/// is not an address Solo's browser opens.
pub fn normalize_address(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    if let Ok(port) = input.parse::<u16>() {
        if port == 0 {
            return None;
        }
        return Some(format!("http://localhost:{port}/"));
    }
    if looks_like_windows_path(input) || input.starts_with('/') && !input.starts_with("//") {
        let url = Url::from_file_path(input).ok()?;
        return is_allowed_in_solo(&url).then(|| url.to_string());
    }
    if let Some((scheme, _)) = input.split_once("://") {
        let url = Url::parse(input).ok()?;
        if !scheme.eq_ignore_ascii_case(url.scheme()) {
            return None;
        }
        return is_allowed_in_solo(&url).then(|| url.to_string());
    }
    if input.eq_ignore_ascii_case("about:blank") {
        return Some("about:blank".to_string());
    }
    if input.contains(char::is_whitespace) {
        return None;
    }
    // A host, optionally with port and path: `localhost:5173/settings`, `example.com`.
    let host_part = input
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let host_name = host_name_of(&host_part);
    let local = host_name == "localhost"
        || host_name.ends_with(".localhost")
        || host_name == "[::1]"
        || host_name
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|address| address.is_loopback());
    let scheme = if local || is_private_address(&host_name) {
        "http"
    } else if host_name.contains('.') || host_name.starts_with('[') {
        "https"
    } else {
        return None;
    };
    let url = Url::parse(&format!("{scheme}://{input}")).ok()?;
    is_allowed_in_solo(&url).then(|| url.to_string())
}

fn host_name_of(host_and_port: &str) -> String {
    if host_and_port.starts_with('[') {
        return host_and_port
            .split_inclusive(']')
            .next()
            .unwrap_or(host_and_port)
            .to_string();
    }
    host_and_port
        .split(':')
        .next()
        .unwrap_or(host_and_port)
        .to_string()
}

fn is_private_address(host: &str) -> bool {
    host.parse::<std::net::Ipv4Addr>()
        .is_ok_and(|address| address.is_private() || address.is_link_local())
}

fn looks_like_windows_path(input: &str) -> bool {
    let bytes = input.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/'))
        || input.starts_with("\\\\")
}

/// What the address bar shows for `url`: no scheme for http(s), no trailing
/// slash on a bare host.
pub fn display_address(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else {
        return url.to_string();
    };
    match parsed.scheme() {
        "http" | "https" => {
            let without_scheme = &url[parsed.scheme().len() + 3..];
            if parsed.path() == "/" && parsed.query().is_none() && parsed.fragment().is_none() {
                without_scheme.trim_end_matches('/').to_string()
            } else {
                without_scheme.to_string()
            }
        }
        _ => url.to_string(),
    }
}

/// Two URLs are the same page when they only differ in a trailing slash or fragment.
pub fn same_page(a: &str, b: &str) -> bool {
    fn key(url: &str) -> String {
        match Url::parse(url) {
            Ok(mut parsed) => {
                parsed.set_fragment(None);
                parsed.to_string().trim_end_matches('/').to_string()
            }
            Err(_) => url.trim_end_matches('/').to_string(),
        }
    }
    key(a) == key(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_and_host_become_local_urls() {
        assert_eq!(
            normalize_address("5173").as_deref(),
            Some("http://localhost:5173/")
        );
        assert_eq!(
            normalize_address("localhost:5173").as_deref(),
            Some("http://localhost:5173/")
        );
        assert_eq!(
            normalize_address(" localhost:5173/settings ").as_deref(),
            Some("http://localhost:5173/settings")
        );
        assert_eq!(
            normalize_address("127.0.0.1:8080").as_deref(),
            Some("http://127.0.0.1:8080/")
        );
        assert_eq!(
            normalize_address("[::1]:3000").as_deref(),
            Some("http://[::1]:3000/")
        );
        assert_eq!(
            normalize_address("app.localhost:3000").as_deref(),
            Some("http://app.localhost:3000/")
        );
        assert_eq!(
            normalize_address("192.168.1.20:8000").as_deref(),
            Some("http://192.168.1.20:8000/")
        );
    }

    #[test]
    fn other_hosts_get_https() {
        assert_eq!(
            normalize_address("example.com").as_deref(),
            Some("https://example.com/")
        );
        assert_eq!(
            normalize_address("example.com/docs?q=1").as_deref(),
            Some("https://example.com/docs?q=1")
        );
        assert_eq!(
            normalize_address("http://example.com").as_deref(),
            Some("http://example.com/")
        );
    }

    #[test]
    fn rejects_what_solo_does_not_open() {
        assert_eq!(normalize_address(""), None);
        assert_eq!(normalize_address("0"), None);
        assert_eq!(normalize_address("hello world"), None);
        assert_eq!(normalize_address("intranet"), None);
        assert_eq!(normalize_address("ftp://example.com"), None);
        assert_eq!(normalize_address("javascript:alert(1)"), None);
        assert_eq!(normalize_address("file:///C:/notes/readme.txt"), None);
    }

    #[cfg(windows)]
    #[test]
    fn html_files_become_file_urls() {
        assert_eq!(
            normalize_address(r"C:\site\index.html").as_deref(),
            Some("file:///C:/site/index.html")
        );
        assert_eq!(normalize_address(r"C:\site\notes.txt"), None);
        assert_eq!(
            normalize_address("file:///C:/site/page.HTM").as_deref(),
            Some("file:///C:/site/page.HTM")
        );
    }

    #[test]
    fn local_addresses() {
        let local = |url: &str| is_local(&Url::parse(url).unwrap());
        assert!(local("http://localhost:5173/"));
        assert!(local("http://LOCALHOST/"));
        assert!(local("http://127.0.0.1:8000/"));
        assert!(local("http://127.1.2.3/"));
        assert!(local("http://[::1]:3000/"));
        assert!(local("http://app.localhost/"));
        assert!(local("file:///C:/site/index.html"));
        assert!(!local("http://localhost.example.com/"));
        assert!(!local("http://192.168.1.2/"));
        assert!(!local("https://example.com/"));
    }

    #[test]
    fn link_targets_per_setting() {
        use LinkTarget::*;
        use OpenLinksIn::*;
        let local = "http://localhost:5173/";
        let remote = "https://example.com/";

        assert_eq!(link_target(local, LocalInSolo, false), Solo);
        assert_eq!(link_target(remote, LocalInSolo, false), System);
        assert_eq!(link_target(local, AllInSolo, false), Solo);
        assert_eq!(link_target(remote, AllInSolo, false), Solo);
        assert_eq!(link_target(local, AllInSystem, false), System);
        assert_eq!(link_target(remote, AllInSystem, false), System);

        // Ctrl+Shift+click opens the other way.
        assert_eq!(link_target(local, LocalInSolo, true), System);
        assert_eq!(link_target(remote, LocalInSolo, true), Solo);
        assert_eq!(link_target(remote, AllInSolo, true), System);
        assert_eq!(link_target(local, AllInSystem, true), Solo);

        // Never in Solo: other schemes.
        assert_eq!(link_target("mailto:a@b.c", AllInSolo, false), System);
        assert_eq!(link_target("vscode://file/x", AllInSolo, true), System);
    }

    #[test]
    fn display_and_same_page() {
        assert_eq!(display_address("http://localhost:5173/"), "localhost:5173");
        assert_eq!(
            display_address("http://localhost:5173/settings"),
            "localhost:5173/settings"
        );
        assert_eq!(display_address("about:blank"), "about:blank");
        assert!(same_page("http://localhost:5173", "http://localhost:5173/"));
        assert!(same_page(
            "http://localhost:5173/a#x",
            "http://localhost:5173/a"
        ));
        assert!(!same_page(
            "http://localhost:5173/a",
            "http://localhost:5173/b"
        ));
    }
}
