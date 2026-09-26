//! agent-device's selector grammar, the subset Extend supports on Windows.
//!
//! A selector is terms separated by spaces, all of which must match: `role="button" label="Save"`.
//! `key="value"` is a case-insensitive exact match, `key~="a|b"` a case-insensitive "contains any of".
//! Boolean keys (`visible`, `editable`, …) stand alone or take `=true` / `=false`. Alternatives are
//! separated by `||`. Text that isn't a selector at all matches labels and values that contain it.

use super::model::RawNode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Id,
    Role,
    Text,
    Label,
    Value,
    AppName,
    WindowTitle,
    Visible,
    Hidden,
    Editable,
    Selected,
    Focused,
    Enabled,
    Hittable,
}

impl Key {
    fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "id" => Self::Id,
            "role" | "type" => Self::Role,
            "text" => Self::Text,
            "label" => Self::Label,
            "value" => Self::Value,
            "appname" => Self::AppName,
            "windowtitle" => Self::WindowTitle,
            "visible" => Self::Visible,
            "hidden" => Self::Hidden,
            "editable" => Self::Editable,
            "selected" => Self::Selected,
            "focused" => Self::Focused,
            "enabled" => Self::Enabled,
            "hittable" => Self::Hittable,
            _ => return None,
        })
    }
    fn is_boolean(self) -> bool {
        matches!(
            self,
            Self::Visible
                | Self::Hidden
                | Self::Editable
                | Self::Selected
                | Self::Focused
                | Self::Enabled
                | Self::Hittable
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    Equals(Key, String),
    Contains(Key, Vec<String>),
    Flag(Key, bool),
    /// Bare text: label, value or id contains it.
    Text(String),
}

/// Alternatives (`||`), each a list of terms that must all match.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub alternatives: Vec<Vec<Term>>,
    pub source: String,
}

/// Splits on spaces outside double quotes; keeps quotes so values can be unquoted afterwards.
fn tokens(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in s.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if quoted {
        return Err(format!("unterminated quote in selector {s:?}"));
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        v[1..v.len() - 1].to_owned()
    } else {
        v.to_owned()
    }
}

fn parse_term(token: &str) -> Result<Option<Term>, String> {
    let (key, op, value) = if let Some((k, v)) = token.split_once("~=") {
        (k, "~=", Some(v))
    } else if let Some((k, v)) = token.split_once('=') {
        (k, "=", Some(v))
    } else {
        (token, "", None)
    };
    let Some(key) = Key::parse(key) else { return Ok(None) };
    match (op, value) {
        ("", None) if key.is_boolean() => Ok(Some(Term::Flag(key, true))),
        ("", None) => Ok(None),
        ("=", Some(v)) if key.is_boolean() => match unquote(v).to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(Some(Term::Flag(key, true))),
            "false" | "0" | "no" => Ok(Some(Term::Flag(key, false))),
            other => Err(format!("{token}: {other:?} isn't true or false")),
        },
        ("=", Some(v)) => Ok(Some(Term::Equals(key, unquote(v)))),
        ("~=", Some(v)) => {
            let parts: Vec<String> = unquote(v)
                .split('|')
                .map(|p| p.trim().to_lowercase())
                .filter(|p| !p.is_empty())
                .collect();
            if parts.is_empty() {
                return Err(format!("{token}: empty pattern"));
            }
            Ok(Some(Term::Contains(key, parts)))
        }
        _ => Ok(None),
    }
}

/// True when `s` uses selector syntax (a known key), rather than being plain text.
pub fn looks_like_selector(s: &str) -> bool {
    let uses_key = |tok: &String| {
        let key = tok.split_once("~=").or_else(|| tok.split_once('=')).map(|(k, _)| k);
        match key {
            Some(k) => Key::parse(k).is_some(),
            None => Key::parse(tok).is_some_and(Key::is_boolean),
        }
    };
    tokens(s).is_ok_and(|t| t.iter().any(uses_key))
}

pub fn parse(s: &str) -> Result<Selector, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err("empty selector".into());
    }
    if tokens(trimmed).is_err() && trimmed.contains('=') {
        return Err(format!("unterminated quote in selector {s:?}"));
    }
    if !looks_like_selector(trimmed) {
        return Ok(Selector {
            alternatives: vec![vec![Term::Text(unquote(trimmed))]],
            source: s.to_owned(),
        });
    }
    let mut alternatives = vec![vec![]];
    for tok in tokens(trimmed)? {
        if tok == "||" {
            alternatives.push(vec![]);
            continue;
        }
        match parse_term(&tok)? {
            Some(term) => alternatives.last_mut().expect("one").push(term),
            None => {
                return Err(format!(
                    "unknown selector term {tok:?}; keys are id, role, text, label, value, appname, windowtitle, visible, hidden, editable, selected, focused, enabled, hittable"
                ));
            }
        }
    }
    if alternatives.iter().any(Vec::is_empty) {
        return Err(format!("empty alternative in selector {s:?}"));
    }
    Ok(Selector {
        alternatives,
        source: s.to_owned(),
    })
}

fn field(node: &RawNode, key: Key) -> Vec<&str> {
    match key {
        Key::Id => vec![node.automation_id.as_str()],
        Key::Role => vec![node.role(), node.type_name()],
        Key::Label => vec![node.name.as_str()],
        Key::Value => vec![node.value.as_deref().unwrap_or("")],
        Key::Text => vec![node.name.as_str(), node.value.as_deref().unwrap_or("")],
        Key::AppName => vec![node.app_name.as_str()],
        Key::WindowTitle => vec![node.window_title.as_str()],
        _ => vec![],
    }
}

fn flag(node: &RawNode, key: Key) -> bool {
    match key {
        Key::Visible => node.visible(),
        Key::Hidden => !node.visible(),
        Key::Editable => node.editable,
        Key::Selected => node.selected == Some(true),
        Key::Focused => node.focused,
        Key::Enabled => node.enabled,
        Key::Hittable => node.hittable(),
        _ => false,
    }
}

fn term_matches(term: &Term, node: &RawNode) -> bool {
    match term {
        Term::Equals(key, v) => {
            let want = v.trim().to_lowercase();
            field(node, *key).iter().any(|f| f.trim().to_lowercase() == want)
        }
        Term::Contains(key, parts) => field(node, *key).iter().any(|f| {
            let f = f.to_lowercase();
            parts.iter().any(|p| f.contains(p.as_str()))
        }),
        Term::Flag(key, want) => flag(node, *key) == *want,
        Term::Text(t) => {
            let t = t.to_lowercase();
            node.name.to_lowercase().contains(&t)
                || node.value.as_deref().is_some_and(|v| v.to_lowercase().contains(&t))
                || (!node.automation_id.is_empty() && node.automation_id.to_lowercase() == t)
        }
    }
}

impl Selector {
    pub fn matches(&self, node: &RawNode) -> bool {
        self.alternatives
            .iter()
            .any(|terms| terms.iter().all(|t| term_matches(t, node)))
    }

    /// Indexes of matching nodes, best first: visible and hittable before hidden, then document order.
    pub fn find_all(&self, raw: &[RawNode]) -> Vec<usize> {
        let mut hits: Vec<usize> = (0..raw.len()).filter(|&i| self.matches(&raw[i])).collect();
        hits.sort_by_key(|&i| (!raw[i].hittable(), !raw[i].visible(), i));
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::{ct, tests::notepad};
    use super::*;

    #[test]
    fn parses_terms() {
        let s = parse(r#"role="button" label="Save As""#).unwrap();
        assert_eq!(
            s.alternatives,
            vec![vec![
                Term::Equals(Key::Role, "button".into()),
                Term::Equals(Key::Label, "Save As".into())
            ]]
        );
        let s = parse(r#"label~="Wi-Fi|Battery" visible"#).unwrap();
        assert_eq!(
            s.alternatives[0],
            vec![
                Term::Contains(Key::Label, vec!["wi-fi".into(), "battery".into()]),
                Term::Flag(Key::Visible, true)
            ]
        );
        let s = parse("editable=false").unwrap();
        assert_eq!(s.alternatives[0], vec![Term::Flag(Key::Editable, false)]);
        let s = parse(r#"id=ok || label="OK""#).unwrap();
        assert_eq!(s.alternatives.len(), 2);
        let s = parse("Continue").unwrap();
        assert_eq!(s.alternatives[0], vec![Term::Text("Continue".into())]);
        let s = parse("\"Sign in\"").unwrap();
        assert_eq!(s.alternatives[0], vec![Term::Text("Sign in".into())]);
        assert!(parse(r#"label="unterminated"#).is_err());
        assert!(parse(r#"role=button bogus=1"#).is_err());
        assert!(parse("visible=maybe").is_err());
        assert!(parse("").is_err());
        assert!(looks_like_selector(r#"label="x""#));
        assert!(!looks_like_selector("Save file"));
    }

    #[test]
    fn matches_nodes() {
        let raw = notepad();
        let close = parse(r#"role="button" label="close""#).unwrap();
        assert_eq!(close.find_all(&raw), vec![7]);
        let menu = parse(r#"role=menu-item label~="fi|ed""#).unwrap();
        assert_eq!(menu.find_all(&raw), vec![5, 6]);
        let text = parse("world").unwrap();
        assert_eq!(text.find_all(&raw), vec![3], "bare text matches values");
        let disabled = parse("enabled=false").unwrap();
        assert_eq!(disabled.find_all(&raw), vec![8]);
        let focused = parse("focused editable").unwrap();
        assert_eq!(focused.find_all(&raw), vec![3]);
        let by_type = parse("role=CheckBox").unwrap();
        assert_eq!(by_type.find_all(&raw), vec![8], "UIA type names work as roles");
        let either = parse("label=File || label=Close").unwrap();
        assert_eq!(either.find_all(&raw), vec![5, 7]);
        let app = parse("appname=notepad windowtitle~=untitled role=window").unwrap();
        assert_eq!(app.find_all(&raw), vec![1]);
    }

    #[test]
    fn visible_matches_rank_first() {
        let mut raw = notepad();
        raw.push(raw[7].clone());
        raw[7].offscreen = true;
        let s = parse("label=Close").unwrap();
        assert_eq!(s.find_all(&raw), vec![10, 7]);
        let hidden = parse("label=Close hidden").unwrap();
        assert_eq!(hidden.find_all(&raw), vec![7]);
        assert_eq!(raw[0].control_type, ct::APPLICATION);
    }
}
