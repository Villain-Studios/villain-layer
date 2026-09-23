pub mod github;
pub mod jira;
pub mod slack;

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
