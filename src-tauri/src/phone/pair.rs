//! Pairing a phone (PHONE-3), and knowing it again by its token.
//!
//! A phone is let in only by someone at the Mac: Settings shows a code, the
//! phone types it, and gets a token of its own. The code is short-lived and
//! allows a handful of tries, since anyone on the network can try it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a code shown in Settings can be used.
pub const CODE_LIFE: Duration = Duration::from_secs(120);
/// Wrong tries before a code is thrown away. A six-digit code with five
/// tries is a one-in-200,000 guess, once per code shown.
pub const TRIES: u8 = 5;

#[derive(Debug)]
pub struct Pairing {
    code: String,
    until: Instant,
    tries_left: u8,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Attempt {
    Paired,
    Wrong,
    /// No code is showing: none was asked for, it ran out, or its tries are
    /// spent.
    NoCode,
}

impl Pairing {
    pub fn new(now: Instant) -> Self {
        Self { code: six_digits(), until: now + CODE_LIFE, tries_left: TRIES }
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn left(&self, now: Instant) -> Duration {
        self.until.saturating_duration_since(now)
    }

    pub fn live(&self, now: Instant) -> bool {
        now < self.until && self.tries_left > 0
    }

    pub fn attempt(&mut self, code: &str, now: Instant) -> Attempt {
        if !self.live(now) {
            return Attempt::NoCode;
        }
        if same(code.trim().as_bytes(), self.code.as_bytes()) {
            // Once: the same code must not pair a second phone.
            self.tries_left = 0;
            return Attempt::Paired;
        }
        self.tries_left -= 1;
        Attempt::Wrong
    }
}

fn six_digits() -> String {
    // v4 uuids are from the OS's random source.
    let n = uuid::Uuid::new_v4().as_u128() % 1_000_000;
    format!("{n:06}")
}

/// A new phone's token: 256 random bits, as hex.
pub fn mint() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// Equal, in time that does not depend on where they first differ.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The phone a token belongs to, comparing against every one.
pub fn holder<'a>(tokens: &'a HashMap<String, String>, token: &str) -> Option<&'a str> {
    let mut found = None;
    for (device, known) in tokens {
        if same(known.as_bytes(), token.as_bytes()) {
            found = Some(device.as_str());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_pairs_once_and_only_while_it_lasts() {
        let now = Instant::now();
        let mut p = Pairing::new(now);
        assert_eq!(p.code().len(), 6);
        assert!(p.code().chars().all(|c| c.is_ascii_digit()));
        let code = p.code().to_string();
        assert_eq!(p.attempt(&format!(" {code} "), now), Attempt::Paired);
        assert_eq!(p.attempt(&code, now), Attempt::NoCode, "one phone per code");

        let mut late = Pairing::new(now);
        let code = late.code().to_string();
        assert_eq!(late.attempt(&code, now + CODE_LIFE), Attempt::NoCode);
    }

    #[test]
    fn five_wrong_tries_spend_a_code() {
        let now = Instant::now();
        let mut p = Pairing::new(now);
        let code = p.code().to_string();
        let wrong = if code == "000000" { "000001" } else { "000000" };
        for _ in 0..TRIES {
            assert_eq!(p.attempt(wrong, now), Attempt::Wrong);
        }
        assert_eq!(p.attempt(&code, now), Attempt::NoCode, "the right code, too late");
    }

    #[test]
    fn a_token_is_known_only_whole() {
        let mut tokens = HashMap::new();
        let token = mint();
        assert_eq!(token.len(), 64);
        tokens.insert("phone-1".to_string(), token.clone());
        assert_eq!(holder(&tokens, &token), Some("phone-1"));
        assert_eq!(holder(&tokens, &token[..63]), None);
        assert_eq!(holder(&tokens, ""), None);
        assert_eq!(holder(&tokens, &mint()), None);
    }
}
