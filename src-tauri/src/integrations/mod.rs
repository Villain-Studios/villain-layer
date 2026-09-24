pub mod github;
pub mod jira;
pub mod slack;

use crate::error::{Error, Result};

/// Every integration client, built the same way.
///
/// The timeout matters: without one, a host that accepts the connection and
/// then goes quiet — a VPN-only GitHub Enterprise, a hung proxy — leaves the
/// command awaiting forever, which the UI shows as a spinner that never
/// resolves and an MCP tool call that never returns.
///
/// One client for the whole process, handed out as clones that share its
/// connection pool. A fresh one per command meant every poll, every ticket
/// opened and every PR sweep paid for a new TCP and TLS handshake — a
/// noticeable part of each call to a site across an ocean.
pub fn http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .user_agent("villain-layer")
                .build()
                .unwrap_or_default()
        })
        .clone()
}

/// Refuse a site whose address would carry a token unencrypted (SET-3).
///
/// Both URLs are typed by the user, and nothing checked them: an `http://`
/// Jira sent `Basic email:token` in the clear on every request, and an
/// `http://` GitHub Enterprise its bearer token. Plain http is allowed only
/// to this machine, where nothing leaves it: a local proxy or test server.
pub fn require_https(url: &str, what: &str) -> Result<()> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| Error::Other(format!("{what} is not a web address: {url}")))?;
    let host = parsed.host_str().unwrap_or("").trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        _ => Err(Error::Other(format!(
            "{what} has to start with https://. Over {}:// the token would travel unencrypted.",
            parsed.scheme()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_goes_only_to_an_https_site_or_to_this_machine() {
        assert!(require_https("https://acme.atlassian.net", "Jira").is_ok());
        assert!(require_https("https://ghe.acme.test/api/v3", "GitHub").is_ok());
        assert!(require_https("http://localhost:8080", "Jira").is_ok());
        assert!(require_https("http://127.0.0.1:9", "Jira").is_ok());
        assert!(require_https("http://[::1]:9/api/v3", "GitHub").is_ok());

        assert!(require_https("http://acme.atlassian.net", "Jira").is_err());
        assert!(require_https("http://localhost.acme.test", "Jira").is_err(), "a name, not this machine");
        assert!(require_https("http://10.0.0.5/api/v3", "GitHub").is_err(), "a private network is still a network");
        assert!(require_https("ftp://acme.test", "Jira").is_err());
        assert!(require_https("acme.atlassian.net", "Jira").is_err());
    }
}
