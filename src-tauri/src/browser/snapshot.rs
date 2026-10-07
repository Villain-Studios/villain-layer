//! A page as an agent reads it: the accessibility tree as an indented
//! outline, each element it can act on numbered (`[ref=e12]`).
//!
//! The tree, not the HTML: it names things as a person using a screen
//! reader would hear them, drops the layout, and is a fraction of the size.
//! A ref is the element's backend node id, which holds for as long as the
//! element is in the document.

use std::collections::HashMap;

use serde_json::Value;

/// An agent reads the whole outline into its context; a page past this is
/// cut, and says so.
const MAX_CHARS: usize = 40_000;

/// Roles an agent acts on, which get a ref whether or not they are focusable.
const ACTIONABLE: &[&str] = &[
    "button", "link", "textbox", "searchbox", "checkbox", "radio", "combobox", "listbox", "option",
    "menuitem", "menuitemcheckbox", "menuitemradio", "tab", "switch", "slider", "spinbutton",
    "treeitem", "textField", "TextField", "ComboBoxGrouping", "ComboBoxMenuButton", "PopUpButton",
];

/// Roles that only wrap others: their children are printed in their place.
const WRAPPERS: &[&str] = &[
    "none", "generic", "GenericContainer", "Section", "presentation", "LineBreak", "InlineTextBox",
    "LayoutTable", "LayoutTableRow", "LayoutTableCell", "Div", "Span", "paragraph", "group", "Pre",
];

struct Node<'a> {
    role: &'a str,
    name: &'a str,
    value: Option<String>,
    ignored: bool,
    backend: Option<i64>,
    children: Vec<&'a str>,
    props: Vec<(&'a str, &'a Value)>,
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.get("value")).and_then(Value::as_str).unwrap_or("")
}

fn parse(nodes: &[Value]) -> (Option<&str>, HashMap<&str, Node<'_>>) {
    let mut map = HashMap::new();
    let mut root = None;
    for n in nodes {
        let Some(id) = n.get("nodeId").and_then(Value::as_str) else { continue };
        if root.is_none() && n.get("parentId").is_none() {
            root = Some(id);
        }
        let value = n.get("value").and_then(|v| v.get("value")).and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Number(x) => Some(x.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        });
        let props = n
            .get("properties")
            .and_then(Value::as_array)
            .map(|ps| {
                ps.iter()
                    .filter_map(|p| Some((p.get("name")?.as_str()?, p.get("value")?.get("value")?)))
                    .collect()
            })
            .unwrap_or_default();
        map.insert(id, Node {
            role: text(n, "role"),
            name: text(n, "name"),
            value,
            ignored: n.get("ignored").and_then(Value::as_bool).unwrap_or(false),
            backend: n.get("backendDOMNodeId").and_then(Value::as_i64),
            children: n
                .get("childIds")
                .and_then(Value::as_array)
                .map(|c| c.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default(),
            props,
        });
    }
    (root, map)
}

fn prop<'a>(n: &'a Node, name: &str) -> Option<&'a Value> {
    n.props.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

fn quote(s: &str) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped: String = one_line.chars().take(200).collect();
    let more = if one_line.chars().count() > 200 { "…" } else { "" };
    format!("\"{}{more}\"", clipped.replace('"', "\\\""))
}

/// Whether a node's text children are already said by its name or its
/// value, so they add nothing: a button's label, a field's contents. The
/// text may sit inside wrappers (a field's own editor is a div).
fn said_already(map: &HashMap<&str, Node>, n: &Node) -> bool {
    fn only_said(map: &HashMap<&str, Node>, id: &str, said: &str) -> bool {
        map.get(id).is_none_or(|k| match k.role {
            "StaticText" | "InlineTextBox" => said.contains(k.name.trim()),
            _ if k.ignored || (WRAPPERS.contains(&k.role) && k.name.is_empty()) => {
                k.children.iter().all(|c| only_said(map, c, said))
            }
            _ => false,
        })
    }
    let said = format!("{} {}", n.name, n.value.as_deref().unwrap_or(""));
    !said.trim().is_empty() && !n.children.is_empty() && n.children.iter().all(|c| only_said(map, c, &said))
}

fn walk(map: &HashMap<&str, Node>, id: &str, depth: usize, out: &mut Vec<String>) {
    let Some(n) = map.get(id) else { return };
    let children = |out: &mut Vec<String>, depth: usize| {
        for c in &n.children {
            walk(map, c, depth, out);
        }
    };

    if n.ignored || (WRAPPERS.contains(&n.role) && n.name.is_empty()) {
        return children(out, depth);
    }
    if n.role == "StaticText" {
        if !n.name.trim().is_empty() {
            out.push(format!("{}- text: {}", "  ".repeat(depth), quote(n.name)));
        }
        return;
    }
    if n.role == "RootWebArea" {
        return children(out, depth);
    }

    let mut line = format!("{}- {}", "  ".repeat(depth), n.role);
    if !n.name.is_empty() {
        line.push(' ');
        line.push_str(&quote(n.name));
    }
    let focusable = prop(n, "focusable").and_then(Value::as_bool).unwrap_or(false);
    if let Some(b) = n.backend.filter(|_| focusable || ACTIONABLE.contains(&n.role)) {
        line.push_str(&format!(" [ref=e{b}]"));
    }
    for (key, shown) in [("checked", "checked"), ("selected", "selected"), ("expanded", "expanded"), ("pressed", "pressed")] {
        match prop(n, key) {
            Some(Value::Bool(true)) => line.push_str(&format!(" [{shown}]")),
            Some(Value::String(s)) if s == "true" || s == "mixed" => line.push_str(&format!(" [{shown}={s}]")),
            _ => {}
        }
    }
    if prop(n, "disabled").and_then(Value::as_bool) == Some(true) {
        line.push_str(" [disabled]");
    }
    if prop(n, "focused").and_then(Value::as_bool) == Some(true) {
        line.push_str(" [focused]");
    }
    if let Some(level) = prop(n, "level").and_then(Value::as_i64) {
        line.push_str(&format!(" [level={level}]"));
    }
    if let Some(v) = n.value.as_deref().filter(|v| !v.is_empty() && *v != n.name) {
        // A password field's value is the user's, and stays with them.
        let protected = prop(n, "protected").and_then(Value::as_bool).unwrap_or(false);
        line.push_str(&format!(": {}", if protected { "\"••••\"".into() } else { quote(v) }));
    }
    if n.role == "link" {
        if let Some(url) = prop(n, "url").and_then(Value::as_str) {
            line.push_str(&format!(" -> {url}"));
        }
    }
    out.push(line);
    if !said_already(map, n) {
        children(out, depth + 1);
    }
}

/// The outline of `nodes`, as `Accessibility.getFullAXTree` returns them.
pub fn outline(nodes: &[Value]) -> String {
    let (root, map) = parse(nodes);
    let mut lines = Vec::new();
    if let Some(root) = root {
        walk(&map, root, 0, &mut lines);
    }
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        if out.len() + line.len() > MAX_CHARS {
            out.push_str(&format!(
                "… cut here: {} more lines. Scroll (browser_scroll) or use browser_evaluate to read further.\n",
                lines.len() - i
            ));
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.is_empty() {
        out.push_str("(the page is empty)\n");
    }
    out
}

/// The backend node id an `e<number>` ref names.
pub fn node_of(r: &str) -> Option<i64> {
    r.trim().trim_start_matches('[').trim_start_matches("ref=").strip_prefix('e')?.trim_end_matches(']').parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, parent: Option<&str>, role: &str, name: &str, backend: i64, children: &[&str]) -> Value {
        let mut n = json!({
            "nodeId": id,
            "role": { "type": "role", "value": role },
            "name": { "type": "computedString", "value": name },
            "backendDOMNodeId": backend,
            "childIds": children,
            "ignored": false,
        });
        if let Some(p) = parent {
            n["parentId"] = json!(p);
        }
        n
    }

    #[test]
    fn a_page_reads_as_an_outline_with_refs_on_what_can_be_acted_on() {
        let mut field = node("5", Some("3"), "textbox", "User name", 41, &[]);
        field["value"] = json!({ "type": "string", "value": "ada" });
        let mut pw = node("6", Some("3"), "textbox", "Password", 42, &[]);
        pw["value"] = json!({ "type": "string", "value": "hunter2" });
        pw["properties"] = json!([{ "name": "protected", "value": { "type": "boolean", "value": true } }]);
        let nodes = vec![
            node("1", None, "RootWebArea", "Sign in", 1, &["2", "3"]),
            node("2", Some("1"), "heading", "Welcome", 10, &["21"]),
            node("21", Some("2"), "StaticText", "Welcome", 11, &[]),
            node("3", Some("1"), "generic", "", 12, &["5", "6", "7"]),
            field,
            pw,
            node("7", Some("3"), "button", "Go", 27, &["71"]),
            node("71", Some("7"), "StaticText", "Go", 28, &[]),
        ];
        assert_eq!(
            outline(&nodes),
            "- heading \"Welcome\"\n\
             - textbox \"User name\" [ref=e41]: \"ada\"\n\
             - textbox \"Password\" [ref=e42]: \"••••\"\n\
             - button \"Go\" [ref=e27]\n"
        );
    }

    #[test]
    fn a_ref_is_read_however_the_agent_wrote_it() {
        assert_eq!(node_of("e27"), Some(27));
        assert_eq!(node_of("[ref=e27]"), Some(27));
        assert_eq!(node_of("ref=e27"), Some(27));
        assert_eq!(node_of("27"), None);
    }
}
