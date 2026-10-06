//! Phone access (PHONE-*): which ways in are open, and which phones are
//! paired. Each phone's token is in the keychain (`secrets`), never here.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Both ways in start closed: open, the Mac answers on the network, and a
/// paired phone can see every terminal's output.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PhoneConfig {
    /// Phones reaching the Mac over Tailscale, from anywhere.
    pub tailscale: bool,
    /// Phones on the home network, or on a VPN into it run by the router
    /// (WireGuard on a GL.iNet, say), which puts them there too.
    pub home: bool,
    /// A paired phone may type into agent panes, not only watch them.
    pub typing: bool,
    pub devices: Vec<PhoneDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneDevice {
    pub id: String,
    /// What the phone called itself when it paired.
    pub name: String,
    pub paired_at: DateTime<Utc>,
}
