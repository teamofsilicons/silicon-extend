//! Turning text into keystrokes for `SendInput`.
//!
//! Printable text goes in as Unicode key events (layout independent). Newlines, tabs and backspace
//! become the real keys, because apps treat a typed `\n` character differently from Enter.

/// Virtual-key codes Bridge sends.
pub mod vk {
    pub const BACK: u16 = 0x08;
    pub const TAB: u16 = 0x09;
    pub const RETURN: u16 = 0x0D;
    pub const CONTROL: u16 = 0x11;
    pub const ESCAPE: u16 = 0x1B;
    pub const DELETE: u16 = 0x2E;
    pub const A: u16 = 0x41;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStroke {
    /// One UTF-16 code unit sent with `KEYEVENTF_UNICODE` (a surrogate pair is two strokes).
    Unicode(u16),
    /// A virtual key pressed and released.
    Key(u16),
    /// A modifier held while a key is pressed (`Ctrl+A`).
    Chord(u16, u16),
}

/// The strokes that type `text`.
pub fn plan(text: &str) -> Vec<KeyStroke> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push(KeyStroke::Key(vk::RETURN));
            }
            '\n' => out.push(KeyStroke::Key(vk::RETURN)),
            '\t' => out.push(KeyStroke::Key(vk::TAB)),
            '\u{8}' => out.push(KeyStroke::Key(vk::BACK)),
            '\u{1b}' => out.push(KeyStroke::Key(vk::ESCAPE)),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push(KeyStroke::Unicode(*unit));
                }
            }
        }
    }
    out
}

/// Select everything in the focused field and delete it (what `fill` does before typing).
pub fn clear_field() -> Vec<KeyStroke> {
    vec![KeyStroke::Chord(vk::CONTROL, vk::A), KeyStroke::Key(vk::DELETE)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_text() {
        assert_eq!(plan("hi"), vec![KeyStroke::Unicode('h' as u16), KeyStroke::Unicode('i' as u16)]);
        assert_eq!(plan("a\nb"), vec![KeyStroke::Unicode(97), KeyStroke::Key(vk::RETURN), KeyStroke::Unicode(98)]);
        assert_eq!(plan("\r\n"), vec![KeyStroke::Key(vk::RETURN)]);
        assert_eq!(plan("\t\u{8}"), vec![KeyStroke::Key(vk::TAB), KeyStroke::Key(vk::BACK)]);
        // An emoji outside the BMP is a surrogate pair.
        assert_eq!(plan("😀"), vec![KeyStroke::Unicode(0xD83D), KeyStroke::Unicode(0xDE00)]);
        assert_eq!(plan("é"), vec![KeyStroke::Unicode(0xE9)]);
        assert!(plan("").is_empty());
        assert_eq!(clear_field(), vec![KeyStroke::Chord(vk::CONTROL, vk::A), KeyStroke::Key(vk::DELETE)]);
    }
}
