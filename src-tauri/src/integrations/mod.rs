pub mod github;
pub mod jira;
pub mod slack;

/// Every integration client, built the same way.
///
/// The timeout matters: without one, a host that accepts the connection and
/// then goes quiet — a VPN-only GitHub Enterprise, a hung proxy — leaves the
/// command awaiting forever, which the UI shows as a spinner that never
/// resolves and an MCP tool call that never returns.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .user_agent("villain-layer")
        .build()
        .unwrap_or_default()
}
