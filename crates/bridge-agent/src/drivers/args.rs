//! Command-line token helpers shared by every desktop driver.
//!
//! A command arrives as `command` plus `args`, the tokens after the command name exactly as the
//! Silicon typed them after `bridge`. Bridge chooses the device and the session, so flags that
//! select either are refused here, before anything runs.

use bridge_protocol::capability::RESERVED_FLAGS;

/// Flags that would let a command reach outside the session Bridge set up: another daemon, another
/// config file, or code loaded from disk. Refused alongside [`RESERVED_FLAGS`].
pub const AGENT_ONLY_FLAGS: &[&str] = &[
    "--config",
    "--remote-config",
    "--reporter",
    "--daemon-base-url",
    "--daemon-auth-token",
    "--daemon-transport",
    "--daemon-server-mode",
    "--tenant",
    "--session-isolation",
    "--session-lock",
    "--run-id",
    "--lease-id",
    "--lease-backend",
    "--state-dir",
];

/// Flags the agent adds itself; a copy from the Silicon is dropped rather than refused.
pub const AGENT_ADDED_FLAGS: &[&str] = &["--json"];

/// The flag name of a token, without any inline `=value`.
pub fn flag_name(token: &str) -> Option<&str> {
    if !token.starts_with('-') || token == "-" || token == "--" || is_number(token) {
        return None;
    }
    Some(token.split_once('=').map_or(token, |(name, _)| name))
}

/// True when `token` looks like a (possibly negative, possibly fractional) number.
pub fn is_number(token: &str) -> bool {
    token.parse::<f64>().is_ok()
}

/// The first token that is a reserved flag, as the Silicon typed it.
pub fn find_refused_flag(args: &[String]) -> Option<&str> {
    for token in args {
        if token == "--" {
            break;
        }
        let Some(name) = flag_name(token) else { continue };
        if RESERVED_FLAGS.contains(&name) || AGENT_ONLY_FLAGS.contains(&name) {
            return Some(name);
        }
    }
    None
}

/// The message for a refused flag. Precise, so the Silicon knows what to drop.
pub fn refused_flag_message(flag: &str) -> String {
    match flag {
        "--platform" | "--device" | "--udid" | "--serial" | "--target" => format!(
            "{flag} can't be used through Bridge: the device is the one this session is on. Remove {flag} and run the command again."
        ),
        "--session" => "--session can't be used here: Bridge runs every command in your Bridge session. Remove --session (use `bridge --session <id>` to pick a Bridge session).".to_owned(),
        _ => format!("{flag} can't be used through Bridge because it would reach outside this session. Remove {flag} and run the command again."),
    }
}

/// Drops flags the agent adds itself (`--json`).
pub fn strip_agent_added(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut after_separator = false;
    for token in args {
        if token == "--" {
            after_separator = true;
        }
        if !after_separator && flag_name(token).is_some_and(|n| AGENT_ADDED_FLAGS.contains(&n)) {
            continue;
        }
        out.push(token.clone());
    }
    out
}

/// Tokens split into positionals and flags, given which flags take a value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedArgs {
    pub positionals: Vec<String>,
    /// Flags in order, with their value when they take one.
    pub flags: Vec<(String, Option<String>)>,
}

impl ParsedArgs {
    pub fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == name)
    }
    pub fn value(&self, name: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(n, _)| n == name).and_then(|(_, v)| v.as_deref())
    }
    pub fn values(&self, name: &str) -> Vec<&str> {
        self.flags.iter().filter(|(n, _)| n == name).filter_map(|(_, v)| v.as_deref()).collect()
    }
    pub fn positional(&self, i: usize) -> Option<&str> {
        self.positionals.get(i).map(String::as_str)
    }
}

/// Splits `args`. A flag in `value_flags` takes the next token (or its inline `=value`); every other
/// flag is a switch. Negative numbers are positionals. Everything after `--` is positional.
pub fn parse(args: &[String], value_flags: &[&str]) -> ParsedArgs {
    let mut out = ParsedArgs::default();
    let mut i = 0;
    let mut after_separator = false;
    while i < args.len() {
        let token = &args[i];
        i += 1;
        if after_separator {
            out.positionals.push(token.clone());
            continue;
        }
        if token == "--" {
            after_separator = true;
            continue;
        }
        let Some(name) = flag_name(token) else {
            out.positionals.push(token.clone());
            continue;
        };
        if let Some((_, inline)) = token.split_once('=') {
            out.flags.push((name.to_owned(), Some(inline.to_owned())));
        } else if value_flags.contains(&name) {
            let value = args.get(i).cloned();
            if value.is_some() {
                i += 1;
            }
            out.flags.push((name.to_owned(), value));
        } else {
            out.flags.push((name.to_owned(), None));
        }
    }
    out
}

/// A file name safe to create inside a work directory: the last path component, with separators
/// and control characters removed. Falls back to `fallback` when nothing usable is left.
pub fn safe_file_name(raw: &str, fallback: &str) -> String {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_owned();
    if cleaned.is_empty() { fallback.to_owned() } else { cleaned.chars().take(200).collect() }
}

/// Ensures `name` ends with one of `extensions` (lowercase, with the dot), adding the first if not.
pub fn with_extension(name: &str, extensions: &[&str]) -> String {
    let lower = name.to_ascii_lowercase();
    if extensions.iter().any(|e| lower.ends_with(e)) {
        name.to_owned()
    } else {
        format!("{name}{}", extensions[0])
    }
}

/// The content type Bridge reports for a produced file, by extension.
pub fn content_type_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "json" => "application/json",
        "xml" => "application/xml",
        "ad" | "log" | "txt" | "ndjson" | "yaml" | "yml" => "text/plain",
        "html" => "text/html",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn reserved_flags_are_found_in_every_form() {
        assert_eq!(find_refused_flag(&s(&["@e2", "--platform", "ios"])), Some("--platform"));
        assert_eq!(find_refused_flag(&s(&["--session=abc"])), Some("--session"));
        assert_eq!(find_refused_flag(&s(&["--udid", "x"])), Some("--udid"));
        assert_eq!(find_refused_flag(&s(&["--state-dir", "/tmp"])), Some("--state-dir"));
        assert_eq!(find_refused_flag(&s(&["--daemon-base-url=http://x"])), Some("--daemon-base-url"));
        assert_eq!(find_refused_flag(&s(&["--config", "a.json"])), Some("--config"));
        assert_eq!(find_refused_flag(&s(&["--reporter", "./x.mjs"])), Some("--reporter"));
        assert_eq!(find_refused_flag(&s(&["-i", "-d", "3"])), None);
        // After `--` everything is text.
        assert_eq!(find_refused_flag(&s(&["--", "--platform"])), None);
        // A value that happens to look like a flag name isn't a flag.
        assert_eq!(find_refused_flag(&s(&["fill", "@e1", "platform"])), None);
        for f in RESERVED_FLAGS {
            assert_eq!(find_refused_flag(&s(&[f])), Some(*f), "{f}");
        }
    }

    #[test]
    fn refused_messages_name_the_flag() {
        for f in RESERVED_FLAGS.iter().chain(AGENT_ONLY_FLAGS) {
            assert!(refused_flag_message(f).contains(f), "{f}");
        }
    }

    #[test]
    fn json_is_stripped_not_refused() {
        assert_eq!(strip_agent_added(&s(&["-i", "--json"])), s(&["-i"]));
        assert_eq!(strip_agent_added(&s(&["--", "--json"])), s(&["--", "--json"]));
    }

    #[test]
    fn parse_splits_flags_and_values() {
        let p = parse(&s(&["page.png", "--scale", "0.5", "--overlay-refs", "--crop-on=label=\"x\""]), &["--scale", "--crop-on"]);
        assert_eq!(p.positionals, s(&["page.png"]));
        assert_eq!(p.value("--scale"), Some("0.5"));
        assert!(p.has("--overlay-refs"));
        assert_eq!(p.value("--crop-on"), Some("label=\"x\""));
        let p = parse(&s(&["down", "-0.5"]), &[]);
        assert_eq!(p.positionals, s(&["down", "-0.5"]));
        let p = parse(&s(&["--env", "A=1", "--env", "B=2", "--", "--cwd"]), &["--env"]);
        assert_eq!(p.values("--env"), vec!["A=1", "B=2"]);
        assert_eq!(p.positionals, s(&["--cwd"]));
    }

    #[test]
    fn file_names_stay_inside_the_workdir() {
        assert_eq!(safe_file_name("../../etc/passwd", "x"), "passwd");
        assert_eq!(safe_file_name("C:\\Users\\a\\shot.png", "x"), "shot.png");
        assert_eq!(safe_file_name("..", "x"), "x");
        assert_eq!(safe_file_name("", "screenshot.png"), "screenshot.png");
        assert_eq!(safe_file_name(".hidden", "x"), "hidden");
        assert_eq!(with_extension("shot", &[".png"]), "shot.png");
        assert_eq!(with_extension("shot.PNG", &[".png"]), "shot.PNG");
        assert_eq!(content_type_for("a.png"), "image/png");
        assert_eq!(content_type_for("a.mp4"), "video/mp4");
        assert_eq!(content_type_for("session.ad"), "text/plain");
    }
}
