//! Tokens live in the macOS keychain, never in config.json.
//!
//! All three integrations share **one** keychain item rather than one each, and
//! it is read at most once per run. macOS prompts per item per process, so three
//! items read on every settings fetch meant a stack of password dialogs on every
//! launch. One item, cached, means at most one prompt — and none at all unless
//! something actually needs a token.

use std::collections::HashMap;

use parking_lot::Mutex;
use std::sync::OnceLock;

use crate::error::Result;

/// The release build's identifier, and the service its tokens were saved under.
const SERVICE: &str = "eu.codevillain.villain-layer";
static SERVICE_NAME: OnceLock<String> = OnceLock::new();

/// Keep tokens under this build's own identifier. The dev build has its own
/// config for the same reason: sharing one keychain item with the installed
/// app, each wrote back the whole bundle it had cached, and whichever wrote
/// last erased a token the other had just saved.
pub fn use_service(identifier: &str) {
    let _ = SERVICE_NAME.set(identifier.to_string());
}

fn service() -> &'static str {
    SERVICE_NAME.get().map(String::as_str).unwrap_or(SERVICE)
}
/// The single item holding every token, as a JSON object.
const BUNDLE: &str = "tokens";

pub const JIRA: &str = "jira-token";
pub const GITHUB: &str = "github-token";
pub const SLACK: &str = "slack-token";

/// The v1 layout: one keychain item per integration.
const LEGACY: [&str; 3] = [JIRA, GITHUB, SLACK];

type Bundle = HashMap<String, String>;

static CACHE: OnceLock<Mutex<Option<Bundle>>> = OnceLock::new();

fn cache() -> &'static Mutex<Option<Bundle>> {
    CACHE.get_or_init(|| Mutex::new(None))
}

fn entry(account: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(service(), account)?)
}

fn read_item(account: &str) -> Result<Option<String>> {
    match entry(account)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_item(account: &str, value: &str) -> Result<()> {
    entry(account)?.set_password(value)?;
    Ok(())
}

fn delete_item(account: &str) -> Result<()> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Read the bundle, falling back to a one-time migration of the v1 per-service
/// items. Cached for the life of the process.
fn load() -> Result<Bundle> {
    let mut guard = cache().lock();
    if let Some(bundle) = guard.as_ref() {
        return Ok(bundle.clone());
    }

    let bundle = match read_item(BUNDLE)? {
        Some(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        None => {
            // Fold any v1 items into the bundle, then retire them. This is the
            // only time a launch can prompt more than once.
            let mut migrated = Bundle::new();
            for account in LEGACY {
                if let Ok(Some(value)) = read_item(account) {
                    migrated.insert(account.to_string(), value);
                }
            }
            if !migrated.is_empty() {
                write_item(BUNDLE, &serde_json::to_string(&migrated)?)?;
                for account in LEGACY {
                    let _ = delete_item(account);
                }
            }
            migrated
        }
    };

    *guard = Some(bundle.clone());
    Ok(bundle)
}

fn store(bundle: Bundle) -> Result<()> {
    if bundle.is_empty() {
        let _ = delete_item(BUNDLE);
    } else {
        write_item(BUNDLE, &serde_json::to_string(&bundle)?)?;
    }
    *cache().lock() = Some(bundle);
    Ok(())
}

pub fn get(key: &str) -> Result<Option<String>> {
    Ok(load()?.get(key).cloned())
}

/// Held across a read-change-write of the bundle.
///
/// Every token is in one item, so a change is read, edit, write back — and
/// two at once, connecting Jira while GitHub reconnects, each wrote back the
/// bundle it had read, and whichever landed second erased the other token.
static WRITING: Mutex<()> = Mutex::new(());

pub fn set(key: &str, secret: &str) -> Result<()> {
    let _writing = WRITING.lock();
    let mut bundle = load()?;
    bundle.insert(key.to_string(), secret.to_string());
    store(bundle)
}

pub fn delete(key: &str) -> Result<()> {
    let _writing = WRITING.lock();
    let mut bundle = load()?;
    bundle.remove(key);
    store(bundle)
}

/// Copy the tokens `from`, another identifier, saved into this build's own
/// item, unless this build has one already (DISK-4). Reading another
/// identifier's item is what prompts, once.
pub fn adopt(from: &str) -> Result<()> {
    let _writing = WRITING.lock();
    if read_item(BUNDLE)?.is_some() {
        return Ok(());
    }
    let saved = match keyring::Entry::new(from, BUNDLE)?.get_password() {
        Ok(v) => v,
        Err(keyring::Error::NoEntry) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    write_item(BUNDLE, &saved)?;
    *cache().lock() = None;
    Ok(())
}
