//! Saved sign-ins (BRW-18..20): the user saves a site, a username and a
//! password; agents have the app fill them into a page, and never see the
//! password.
//!
//! An agent's context ends up in transcripts and logs, so the password goes
//! from the keychain to the page's password field without passing through
//! anything an agent reads: not the tool's answer, not the outline, not a
//! screenshot, not a script's value.

use serde_json::Value;
use tauri::{AppHandle, Runtime};

use super::{sites, Browser, Page};
use crate::commands::AppState;
use crate::config::SavedSignIn;
use crate::error::{Error, Result};
use crate::secrets;

/// A site as a sign-in is saved for: a lowercased host, with `:port` if one
/// was given. From a host or a URL.
pub fn site_of(typed: &str) -> Option<String> {
    let typed = typed.trim();
    let url = if typed.contains("://") { typed.to_string() } else { format!("http://{typed}") };
    let host = sites::host_of(&url)?;
    let port = port_of(&url);
    let valid = host.contains('.') || sites::is_local(&host);
    let clean = host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
    if !valid || !clean {
        return None;
    }
    let host = if host.contains(':') { format!("[{host}]") } else { host };
    Some(match port {
        Some(p) => format!("{host}:{p}"),
        None => host,
    })
}

/// The port a URL names, if it names one.
fn port_of(url: &str) -> Option<u16> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let port = match authority.strip_prefix('[') {
        Some(v6) => v6.split_once(']')?.1.strip_prefix(':')?,
        None => authority.split_once(':')?.1,
    };
    port.parse().ok()
}

/// Whether a sign-in saved for `site` is for the page at `url` (BRW-18): the
/// same host, and the same port, or any port of this machine when the site
/// names none, or the default port elsewhere.
pub fn is_for(site: &str, url: &str) -> bool {
    let Some(saved) = site_of(site).map(|s| format!("http://{s}/")) else { return false };
    let (Some(saved_host), Some(host)) = (sites::host_of(&saved), sites::host_of(url)) else { return false };
    if saved_host != host {
        return false;
    }
    match (port_of(&saved), port_of(url)) {
        (Some(want), have) => have == Some(want),
        (None, have) => sites::is_local(&host) || have.is_none(),
    }
}

impl Browser {
    /// The page holds a password the app filled in (BRW-19), in the field
    /// with this backend node id.
    pub fn secret_on_page(&self, task: &str) -> Option<i64> {
        self.inner.lock().active(task).and_then(|t| t.secret)
    }

    fn mark_secret(&self, task: &str, node: i64) {
        if let Some(t) = self.inner.lock().active_mut(task) {
            t.secret = Some(node);
        }
    }
}

/// The saved sign-ins for the page at `url`.
pub fn saved_for(state: &AppState, url: &str) -> Vec<SavedSignIn> {
    state.config.read().browser.sign_ins.into_iter().filter(|s| is_for(&s.site, url)).collect()
}

/// Save a sign-in: its password to the keychain, the rest to the config.
/// Blocking: the keychain can stop and ask.
pub fn save(state: &AppState, site: &str, username: &str, password: &str) -> Result<SavedSignIn> {
    let site = site_of(site).ok_or_else(|| Error::Other(format!("{site} is not a site: give one like localhost or app.example.com")))?;
    let username = username.trim();
    if username.is_empty() || password.is_empty() {
        return Err(Error::Other("a sign-in needs a username and a password".into()));
    }
    // The same account on the same site is replaced, not saved twice.
    let existing = state
        .config
        .read()
        .browser
        .sign_ins
        .into_iter()
        .find(|s| s.site == site && s.username == username);
    let saved = existing.unwrap_or_else(|| SavedSignIn {
        id: uuid::Uuid::new_v4().simple().to_string(),
        site,
        username: username.to_string(),
    });
    secrets::set(&secrets::sign_in_key(&saved.id), password)?;
    let kept = saved.clone();
    state.config.update(|c| {
        if !c.browser.sign_ins.iter().any(|s| s.id == kept.id) {
            c.browser.sign_ins.push(kept);
        }
    })?;
    Ok(saved)
}

/// Forget a sign-in, its password with it. Blocking.
pub fn forget(state: &AppState, id: &str) -> Result<()> {
    secrets::delete(&secrets::sign_in_key(id))?;
    state.config.update(|c| c.browser.sign_ins.retain(|s| s.id != id))
}

/// Type `text` into a field, replacing what it held.
async fn fill(page: &Page, node: i64, text: &str) -> Result<()> {
    page.focus(node).await?;
    page.call_on(node, super::page::SELECT_ALL, vec![]).await?;
    page.insert_text(text).await
}

/// `browser_sign_in` (BRW-19): fill the saved sign-in for the page into the
/// fields the agent named. What the agent is told names the account, never
/// the password.
pub(super) async fn sign_in<R: Runtime>(
    app: &AppHandle<R>,
    state: &AppState,
    page: &Page,
    url: &str,
    args: &Value,
) -> Result<String> {
    let node = |key: &str| -> Result<Option<i64>> {
        match args.get(key).and_then(Value::as_str).filter(|r| !r.is_empty()) {
            None => Ok(None),
            Some(r) => super::snapshot::node_of(r)
                .map(Some)
                .ok_or_else(|| Error::Other(format!("{r} is not a ref: use one from the outline, like e12"))),
        }
    };
    let (user_field, password_field) = (node("username_field")?, node("password_field")?);
    if user_field.is_none() && password_field.is_none() {
        return Err(Error::Other("name the username field, the password field, or both".into()));
    }

    let saved = saved_for(state, url);
    let site = sites::host_of(url).unwrap_or_default();
    let wanted = args.get("username").and_then(Value::as_str).filter(|u| !u.is_empty());
    let account = match wanted {
        Some(u) => saved.iter().find(|s| s.username == u).cloned().ok_or_else(|| {
            Error::Other(format!(
                "no sign-in for {u} is saved for {site}; saved: {}",
                saved.iter().map(|s| s.username.as_str()).collect::<Vec<_>>().join(", ")
            ))
        })?,
        None => saved.first().cloned().ok_or_else(|| {
            Error::Other(format!(
                "no sign-in is saved for {site}. Ask the user to sign in in the browser themselves, or to save \
                 one: Settings → Browser, or Save sign-in in the task's browser after typing it into the page."
            ))
        })?,
    };

    if let Some(n) = user_field {
        fill(page, n, &account.username).await?;
    }
    if let Some(n) = password_field {
        if url.starts_with("http://") && !sites::is_local(&site) {
            return Err(Error::Other(format!(
                "{site} is served over plain http, where a password travels readable: it is filled only over https"
            )));
        }
        // Only into a password field: put into a text box, it could be read
        // back from the outline.
        let kind = page.call_on(n, "function () { return this instanceof HTMLInputElement ? this.type : '' }", vec![]).await?;
        if kind.as_str() != Some("password") {
            return Err(Error::Other(format!("e{n} is not a password field: a password goes only into one")));
        }
        let key = secrets::sign_in_key(&account.id);
        let password = crate::commands::off_runtime(move || secrets::get(&key))
            .await??
            .ok_or_else(|| Error::Other(format!("the password saved for {} on {site} is gone from the keychain", account.username)))?;
        fill(page, n, &password).await?;
        state.browser.mark_secret(&page.task, n);
    }
    if args.get("submit").and_then(Value::as_bool).unwrap_or(false) {
        page.key("Enter").await?;
    }
    let shown = if user_field.is_some() { account.username.as_str() } else { "the saved account" };
    state.browser.record(app, &page.task, &format!("Signed in as {shown}"), None);
    Ok(format!(
        "Filled in the saved sign-in for {} on {site}{}.",
        account.username,
        match (user_field, password_field) {
            (Some(_), Some(_)) => "",
            (Some(_), None) => " (the username)",
            _ => " (the password)",
        }
    ))
}

/// What the page's sign-in form holds, for "Save sign-in" in the panel
/// (BRW-20): the password from its password field, read here and never sent
/// to the window, and the username field's value if the form has one.
pub async fn read_form(page: &Page) -> Result<(Option<String>, String)> {
    let found = page
        .eval(
            "(() => {
               const pw = [...document.querySelectorAll('input[type=password]')].find((i) => i.value);
               if (!pw) return null;
               const scope = pw.form || document;
               const fields = [...scope.querySelectorAll('input')].filter((i) =>
                 i !== pw && i.value && ['text', 'email', 'tel', ''].includes(i.type));
               const named = fields.find((i) => /user|mail|login/i.test(i.autocomplete + i.name + i.id)) || fields[0];
               return { user: named ? named.value : null, password: pw.value };
             })()",
        )
        .await?;
    let password = found
        .get("password")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .ok_or_else(|| Error::Other("type the password into the page's sign-in form first".into()))?
        .to_string();
    let user = found.get("user").and_then(Value::as_str).filter(|u| !u.is_empty()).map(str::to_string);
    Ok((user, password))
}

/// The site a sign-in for the page at `url` is saved for by default: this
/// machine's for every port, a site elsewhere as it is.
pub fn default_site(url: &str) -> Option<String> {
    let host = sites::host_of(url)?;
    if sites::is_local(&host) {
        return Some(if host.contains(':') { format!("[{host}]") } else { host });
    }
    site_of(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tests::{app_with_task, serve};
    use crate::browser::tools::call;
    use serde_json::json;
    use std::time::Duration;
    use tauri::Manager;

    const LOGIN: &str = r#"<html><head><title>Sign in</title></head><body><form onsubmit="event.preventDefault()">
        <input aria-label="Email" type="email" name="email">
        <input aria-label="Password" type="password" id="pw">
        <button type="button" onclick="const p = document.getElementById('pw'); p.type = p.type === 'password' ? 'text' : 'password'">Show password</button>
        <button type="button" onclick="location.href = '/two?ok=' + (document.getElementById('pw').value === 's3cret-Pw')">Sign in</button>
    </form></body></html>"#;

    fn text(content: &[Value]) -> String {
        content.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect()
    }

    fn ref_of(outline: &str, what: &str) -> String {
        let at = outline.find(what).unwrap_or_else(|| panic!("no {what} in {outline}")) + what.len();
        outline[at..].split("[ref=").nth(1).unwrap().split(']').next().unwrap().to_string()
    }

    /// Against a real Chrome, the whole of BRW-19: an agent signs in with a
    /// saved account and the password is nowhere it can read: not the
    /// answer, not the outline, not when the page shows the password, not a
    /// script or a screenshot until the page moves on. It goes only into a
    /// password field. And the sign-in works. The keychain is the tests'
    /// own, in memory.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_agent_signs_in_without_ever_seeing_the_password() {
        let Some((app, root, pane)) = app_with_task() else { return };
        let state = app.state::<AppState>();
        save(&state, "localhost", "ada@acme.test", "s3cret-Pw").unwrap();
        let base = serve(LOGIN).await;
        let handle = app.handle();
        let tool = |name: &'static str, args: Value| call(handle, name, args, Some(&pane));
        let secret = "s3cret-Pw";

        let form = text(&tool("browser_navigate", json!({ "url": format!("{base}/") })).await.unwrap());
        let (email, pw) = (ref_of(&form, "textbox \"Email\""), ref_of(&form, "textbox \"Password\""));

        let wrong = tool("browser_sign_in", json!({ "password_field": email })).await.unwrap_err();
        assert!(wrong.to_string().contains("not a password field"), "{wrong}");

        let signed = text(&tool("browser_sign_in", json!({ "username_field": email, "password_field": pw })).await.unwrap());
        assert!(signed.starts_with("Filled in the saved sign-in for ada@acme.test on localhost."), "{signed}");
        assert!(signed.contains(": \"ada@acme.test\""), "the username is filled: {signed}");
        assert!(!signed.contains(secret), "{signed}");

        for refused in ["browser_evaluate", "browser_screenshot"] {
            let e = tool(refused, json!({ "expression": "document.getElementById('pw').value" })).await.unwrap_err();
            assert!(e.to_string().contains("a saved password is filled into this page"), "{refused}: {e}");
        }
        let shown = text(&tool("browser_click", json!({ "ref": ref_of(&form, "button \"Show password\"") })).await.unwrap());
        assert!(!shown.contains(secret), "hidden even when the page shows it: {shown}");

        let done = text(&tool("browser_click", json!({ "ref": ref_of(&form, "button \"Sign in\"") })).await.unwrap());
        assert!(done.contains("ok=true"), "the password was the saved one: {done}");
        let after = text(&tool("browser_evaluate", json!({ "expression": "location.search" })).await.unwrap());
        assert!(after.contains("ok=true"), "a new page holds no password: {after}");

        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Against a real Chrome: "Save sign-in" in the panel reads what was
    /// typed into the page's form, the username beside the password.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sign_in_typed_into_the_page_is_read_from_its_form() {
        let Some((app, root, _)) = app_with_task() else { return };
        let state = app.state::<AppState>();
        let base = serve(LOGIN).await;
        let page = state.browser.page(app.handle(), "t1").await.unwrap();
        page.navigate(&format!("{base}/")).await.unwrap();
        page.settle(&state.browser).await;
        assert!(read_form(&page).await.unwrap_err().to_string().contains("type the password"));
        page.eval("document.querySelector('[name=email]').value = 'bo@acme.test'; document.getElementById('pw').value = 'typed-pw'")
            .await
            .unwrap();
        assert_eq!(read_form(&page).await.unwrap(), (Some("bo@acme.test".into()), "typed-pw".into()));

        state.browser.shutdown(Duration::from_secs(3));
        state.ptys.shutdown(Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_site_is_kept_as_its_host_and_any_port_given() {
        assert_eq!(site_of("http://LocalHost:4210/login").as_deref(), Some("localhost:4210"));
        assert_eq!(site_of("localhost").as_deref(), Some("localhost"));
        assert_eq!(site_of("app.example.com").as_deref(), Some("app.example.com"));
        assert_eq!(site_of("https://app.example.com:8443/x").as_deref(), Some("app.example.com:8443"));
        assert_eq!(site_of("not a site"), None);
    }

    #[test]
    fn localhost_alone_is_every_port_of_this_machine_and_a_site_elsewhere_its_default_port() {
        assert!(is_for("localhost", "http://localhost:4200/login"));
        assert!(is_for("localhost", "http://localhost:4210/login"));
        assert!(is_for("localhost:4210", "http://localhost:4210/"));
        assert!(!is_for("localhost:4210", "http://localhost:4200/"));
        assert!(is_for("app.example.com", "https://app.example.com/login"));
        assert!(!is_for("app.example.com", "https://app.example.com:8443/login"));
        assert!(!is_for("example.com", "https://app.example.com/"), "a subdomain is another site");
        assert!(!is_for("app.example.com", "https://evil.test/?app.example.com"));
        assert!(!is_for("localhost", "http://localhost@evil.test/"));
    }

    #[test]
    fn a_page_on_this_machine_saves_for_every_port_and_one_elsewhere_for_its_own() {
        assert_eq!(default_site("http://localhost:4210/login").as_deref(), Some("localhost"));
        assert_eq!(default_site("https://app.example.com/login").as_deref(), Some("app.example.com"));
    }
}
