//! What the CLI prints: text for a reader, or with `--json` one JSON document, the way every
//! Silicon Apps CLI does it: on success the data itself on stdout (`extend accounts --json` →
//! `{"app_id": "extend", …}`), on failure `{"error": {…}}` on stderr.
//!
//! Colour follows the `color` setting: `auto` (the default) colours a stream only when it is a
//! terminal and `NO_COLOR` is not set; `always` and `never` override both (no-color.org: a
//! setting the user made wins over `NO_COLOR`).

use std::io::{IsTerminal as _, Write as _};

use serde_json::Value;

// `println!`, `print!`, `eprintln!` and `eprint!` that never panic. When whoever reads the output
// has gone (`extend device ls | head -1`, `extend -v device ls 2>&1 | grep -q …`), a write fails
// with a broken pipe, and the std macros panic on that: exit 101 and a panic message instead of
// the command's own result and exit code. Nobody is left to read that write, so it is dropped and
// the command finishes as it would have.
macro_rules! outln {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}
macro_rules! out {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = write!(std::io::stdout(), $($t)*);
    }};
}
macro_rules! errln {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($t)*);
    }};
}
macro_rules! errout {
    ($($t:tt)*) => {{
        use std::io::Write as _;
        let _ = write!(std::io::stderr(), $($t)*);
    }};
}

use crate::error::CliError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Out,
    Err,
}

/// Whether to colour one stream.
pub fn wants_color(setting: Option<&str>, no_color: Option<&str>, term: Option<&str>, is_tty: bool) -> bool {
    match setting {
        Some("always") => true,
        Some("never") => false,
        _ => is_tty && no_color.is_none_or(str::is_empty) && term != Some("dumb"),
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Colors {
    out: bool,
    err: bool,
}

impl Colors {
    pub fn detect(setting: Option<&str>) -> Self {
        let no_color = std::env::var("NO_COLOR").ok();
        let term = std::env::var("TERM").ok();
        let pick = |tty: bool| wants_color(setting, no_color.as_deref(), term.as_deref(), tty);
        Self {
            out: pick(std::io::stdout().is_terminal()),
            err: pick(std::io::stderr().is_terminal()),
        }
    }

    /// `text` in an SGR style (`"31"` red, `"33"` yellow, `"32"` green, `"36"` cyan, `"2"` dim,
    /// `"1"` bold), when that stream is coloured.
    pub fn paint(&self, stream: Stream, sgr: &str, text: &str) -> String {
        let on = match stream {
            Stream::Out => self.out,
            Stream::Err => self.err,
        };
        if on {
            format!("\x1b[{sgr}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Out {
    pub json: bool,
    pub colors: Colors,
    pub verbose: bool,
}

impl Out {
    /// Prints a command's result: `data` with `--json`, else `text()`.
    pub fn emit(&self, data: Value, text: impl FnOnce() -> String) {
        if self.json {
            outln!("{}", serde_json::to_string(&data).unwrap_or_default());
        } else {
            let t = text();
            if !t.trim().is_empty() {
                outln!("{}", t.trim_end());
            }
        }
    }

    /// Prints a failure and returns its exit code.
    pub fn fail(&self, e: &CliError) -> i32 {
        let mut err = std::io::stderr();
        if self.json {
            let _ = writeln!(err, "{}", e.to_json());
        } else {
            let c = self.colors;
            let _ = writeln!(err, "{} {}", c.paint(Stream::Err, "1;31", "error:"), e.message);
            if let Some(h) = &e.hint {
                let _ = writeln!(err, "  {} {h}", c.paint(Stream::Err, "36", "hint:"));
            }
            let _ = writeln!(
                err,
                "{}",
                c.paint(
                    Stream::Err,
                    "2",
                    &format!(
                        "  code: {} (exit {}){}",
                        e.code.as_str(),
                        e.exit(),
                        e.request_id
                            .as_ref()
                            .map(|r| format!(", request {r}"))
                            .unwrap_or_default()
                    )
                )
            );
        }
        e.exit()
    }

    /// A problem that didn't stop the command, on stderr (text mode only; `--json` carries it in
    /// the data).
    pub fn warn(&self, message: &str) {
        if !self.json {
            errln!("{} {message}", self.colors.paint(Stream::Err, "1;33", "warning:"));
        }
    }

    /// Something worth knowing that is not a problem, on stderr (text mode only; `--json` carries
    /// it in the data where it matters).
    pub fn note(&self, message: &str) {
        if !self.json {
            errln!("{} {message}", self.colors.paint(Stream::Err, "36", "note:"));
        }
    }

    /// `-v`: one line on stderr.
    pub fn verbose(&self, line: impl FnOnce() -> String) {
        if self.verbose {
            errln!(
                "{}",
                self.colors.paint(Stream::Err, "2", &format!("[extend] {}", line()))
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_follows_the_setting_then_no_color_then_the_terminal() {
        // auto: only a terminal, and NO_COLOR (when set to anything) turns it off.
        assert!(wants_color(None, None, Some("xterm-256color"), true));
        assert!(wants_color(Some("auto"), None, None, true));
        assert!(!wants_color(None, None, None, false));
        assert!(!wants_color(None, Some("1"), None, true));
        assert!(wants_color(None, Some(""), None, true), "an empty NO_COLOR is not set");
        assert!(!wants_color(None, None, Some("dumb"), true));
        // A setting the user made wins.
        assert!(wants_color(Some("always"), Some("1"), None, false));
        assert!(!wants_color(Some("never"), None, None, true));
    }

    #[test]
    fn paint_only_when_coloured() {
        let on = Colors { out: true, err: false };
        assert_eq!(on.paint(Stream::Out, "31", "x"), "\x1b[31mx\x1b[0m");
        assert_eq!(on.paint(Stream::Err, "31", "x"), "x");
    }
}
