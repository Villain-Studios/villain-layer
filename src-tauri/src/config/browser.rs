//! What `config.json` keeps for the browser (§18).

use serde::{Deserialize, Serialize};

/// The browser agents use (§18).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BrowserConfig {
    /// Sites agents may use besides this machine's own (BRW-3), as bare
    /// hosts; each covers its subdomains. Empty by default: a page's text is
    /// written by whoever runs the site, and agents read it as instructions.
    pub sites: Vec<String>,
    /// Sign-ins agents may have filled in (BRW-18). Each password is in the
    /// keychain, under `secrets::sign_in_key(id)`, never here.
    pub sign_ins: Vec<SavedSignIn>,
}

/// A saved sign-in, without its password (BRW-18).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedSignIn {
    pub id: String,
    /// `host` or `host:port`; `localhost` alone is every port of it.
    pub site: String,
    pub username: String,
}

/// A task's browser tabs, as kept for next time (BRW-6).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTabs {
    /// Each tab's page, in order; one that had none is `about:blank`.
    pub urls: Vec<String>,
    pub active: usize,
}
