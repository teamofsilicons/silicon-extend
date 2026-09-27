//! Local files sent along with a command: a replay script, an image for a TV, an APK, an
//! `adb push` source.
//!
//! A command carries its files inside the request ([`CommandRequest::attachments`]), so Extend
//! limits them: at most [`MAX_ATTACHMENTS`] files and [`MAX_ATTACHMENT_BYTES`] (8 MiB) in total.
//! Sizes are checked before a file is read, so a huge file is refused without loading it.
//!
//! ```no_run
//! use silicon_extend_client::attachments::{self, AttachmentExt as _};
//! use silicon_extend_client::protocol::model::{Attachment, CommandRequest};
//!
//! # fn demo() -> Result<(), attachments::AttachmentError> {
//! // One file, named after itself:
//! let script = Attachment::from_path("flows/login.ad")?;
//!
//! // Or let the command's own arguments say which are local files: each one is read and
//! // replaced with `attachment:<name>`, which is how the device finds it.
//! let mut args = vec!["com.example.app".to_owned(), "./app.apk".to_owned()];
//! let files = attachments::attach_local_files("install", &mut args)?;
//! assert_eq!(args[1], "attachment:app.apk");
//! let request = CommandRequest {
//!     command: "install".into(),
//!     args,
//!     timeout_ms: None,
//!     self_destruct_minutes: None,
//!     permanent: false,
//!     attachments: files,
//! };
//! # let _ = (script, request);
//! # Ok(()) }
//! ```
//!
//! [`CommandRequest::attachments`]: extend_protocol::model::CommandRequest::attachments

use std::io::Read as _;
use std::path::Path;

use extend_protocol::model::Attachment;

/// The most files one command can carry.
pub const MAX_ATTACHMENTS: usize = 8;
/// The most bytes (before encoding) one command's files can add up to: 8 MiB.
pub const MAX_ATTACHMENT_BYTES: u64 = 8 << 20;
/// How an argument names a file sent with the command.
pub const ATTACHMENT_PREFIX: &str = "attachment:";

/// What was found where a local file was expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Found {
    /// Nothing exists at that path.
    Nothing,
    /// A directory.
    Directory,
    /// Something that isn't a regular file (a device, a pipe, a socket).
    NotRegular,
}

/// Why a local file can't be sent with a command. Each message says what happened, why, and what
/// to do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttachmentError {
    /// The path, as given, doesn't name a regular file on this computer.
    #[error("{}", not_a_file_message(.path, *.found))]
    NotAFile { path: String, found: Found },
    /// The file would take the command past [`MAX_ATTACHMENT_BYTES`]. `already` is what the
    /// command's earlier files add up to.
    #[error(
        "{path} is {size} bytes{}; a command can carry at most {MAX_ATTACHMENT_BYTES} bytes (8 MiB) of local files. \
         Send a smaller file, or split it and send the parts in several commands.",
        if *.already > 0 { format!(", and the command's other files already take {} bytes", .already) } else { String::new() }
    )]
    TooLarge { path: String, size: u64, already: u64 },
    /// The command already carries [`MAX_ATTACHMENTS`] files.
    #[error(
        "{path} would be file number {}, and a command can carry at most {MAX_ATTACHMENTS} local files. \
         Send them in several commands with fewer files each.",
        MAX_ATTACHMENTS + 1
    )]
    TooMany { path: String },
    /// An Android App Bundle where an APK is needed.
    #[error(
        "{path} is an Android App Bundle (.aab); Android installs APKs, and Extend can't turn a bundle into one. \
         Build a universal APK with bundletool (`bundletool build-apks --mode=universal`) and send that."
    )]
    AppBundle { path: String },
    /// The file exists but couldn't be read.
    #[error("Could not read {path}: {error}. Check that the file is readable by this user.")]
    Unreadable { path: String, error: String },
}

fn not_a_file_message(path: &str, found: Found) -> String {
    match found {
        Found::Nothing => format!(
            "There is no file at {path} on this computer, and the command needs a local file there. \
             Check the path (a relative path starts at the current directory)."
        ),
        Found::Directory => format!(
            "{path} is a directory, and the command sends one file. Pass a file, or pack the directory into one \
             (for example with tar) and send that."
        ),
        Found::NotRegular => format!(
            "{path} is not a regular file (it may be a device or a pipe), and the command sends a regular file. \
             Copy it into a regular file first."
        ),
    }
}

impl From<AttachmentError> for crate::Error {
    fn from(e: AttachmentError) -> Self {
        crate::Error::Invalid(e.to_string())
    }
}

/// Building [`Attachment`]s from local files and bytes.
///
/// `Attachment` is a wire type from `extend-protocol`; bring this trait into scope to call
/// `Attachment::from_path(…)`.
pub trait AttachmentExt: Sized {
    /// Reads one local file, named after the file, refusing one over [`MAX_ATTACHMENT_BYTES`]
    /// before reading it.
    fn from_path(path: impl AsRef<Path>) -> Result<Self, AttachmentError>;
    /// An attachment from bytes already in memory. The content type follows the name's extension.
    fn from_bytes(name: impl Into<String>, bytes: &[u8]) -> Self;
    /// The size of the file it carries, in bytes (before encoding).
    fn size(&self) -> u64;
}

impl AttachmentExt for Attachment {
    fn from_path(path: impl AsRef<Path>) -> Result<Self, AttachmentError> {
        let mut set = AttachmentSet::new();
        set.add_path(path)?;
        Ok(set.into_vec().remove(0))
    }

    fn from_bytes(name: impl Into<String>, bytes: &[u8]) -> Self {
        let name = name.into();
        Attachment {
            content_type: content_type_for(&name).to_owned(),
            name,
            content_base64: base64_encode(bytes),
        }
    }

    fn size(&self) -> u64 {
        let s = self.content_base64.trim_end();
        let padding = s.bytes().rev().take_while(|b| *b == b'=').count() as u64;
        (s.len() as u64 / 4 * 3).saturating_sub(padding)
    }
}

/// The files one command carries, kept within [`MAX_ATTACHMENTS`] and [`MAX_ATTACHMENT_BYTES`].
/// Names are made unique (`flow.ad`, `flow-2.ad`), so each `attachment:<name>` finds one file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttachmentSet {
    items: Vec<Attachment>,
    total: u64,
}

impl AttachmentSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads a local file into the set and returns the argument that names it on the device
    /// (`attachment:<name>`). Its size is checked before it's read, and it's read only up to what
    /// the set has room for, in case it grew since it was measured.
    pub fn add_path(&mut self, path: impl AsRef<Path>) -> Result<String, AttachmentError> {
        let path = path.as_ref();
        let shown = path.display().to_string();
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(AttachmentError::NotAFile {
                    path: shown,
                    found: Found::Nothing,
                });
            }
            Err(e) => {
                return Err(AttachmentError::Unreadable {
                    path: shown,
                    error: e.to_string(),
                });
            }
        };
        if meta.is_dir() {
            return Err(AttachmentError::NotAFile {
                path: shown,
                found: Found::Directory,
            });
        }
        if !meta.is_file() {
            return Err(AttachmentError::NotAFile {
                path: shown,
                found: Found::NotRegular,
            });
        }
        if self.items.len() >= MAX_ATTACHMENTS {
            return Err(AttachmentError::TooMany { path: shown });
        }
        let budget = MAX_ATTACHMENT_BYTES - self.total;
        let too_large = |size: u64| AttachmentError::TooLarge {
            path: shown.clone(),
            size,
            already: self.total,
        };
        if meta.len() > budget {
            return Err(too_large(meta.len()));
        }
        // Read at most one byte past the budget, in case the file grew since it was measured.
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        std::fs::File::open(path)
            .and_then(|f| f.take(budget + 1).read_to_end(&mut bytes))
            .map_err(|e| AttachmentError::Unreadable {
                path: shown.clone(),
                error: e.to_string(),
            })?;
        if bytes.len() as u64 > budget {
            return Err(too_large(bytes.len() as u64));
        }
        let name = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        self.push(Attachment::from_bytes(name, &bytes), bytes.len() as u64)
    }

    /// Adds an attachment built elsewhere and returns the argument that names it on the device.
    pub fn add(&mut self, attachment: Attachment) -> Result<String, AttachmentError> {
        let size = attachment.size();
        if self.items.len() >= MAX_ATTACHMENTS {
            return Err(AttachmentError::TooMany {
                path: attachment.name.clone(),
            });
        }
        if size > MAX_ATTACHMENT_BYTES - self.total {
            return Err(AttachmentError::TooLarge {
                path: attachment.name.clone(),
                size,
                already: self.total,
            });
        }
        self.push(attachment, size)
    }

    fn push(&mut self, mut attachment: Attachment, size: u64) -> Result<String, AttachmentError> {
        attachment.name = self.unique_name(&attachment.name);
        self.total += size;
        let reference = format!("{ATTACHMENT_PREFIX}{}", attachment.name);
        self.items.push(attachment);
        Ok(reference)
    }

    fn unique_name(&self, name: &str) -> String {
        let taken = |n: &str| self.items.iter().any(|a| a.name == n);
        if !taken(name) {
            return name.to_owned();
        }
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
            _ => (name, String::new()),
        };
        (2..)
            .map(|n| format!("{stem}-{n}{ext}"))
            .find(|n| !taken(n))
            .unwrap_or_else(|| name.to_owned())
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    /// Bytes carried so far (before encoding).
    pub fn total_bytes(&self) -> u64 {
        self.total
    }
    pub fn into_vec(self) -> Vec<Attachment> {
        self.items
    }
}

/// How a command's argument relates to a file on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalInput {
    /// The device's own argument, sent as typed (shell arguments, device paths, package names).
    No,
    /// Sent along when it names a local file; otherwise it's the device's (a link, say).
    IfFile,
    /// Must name a local file: the device takes this input only as a file sent with the command.
    Required,
}

/// Whether `args[i]` of `command` names a local file sent along with it. Only these are ever read:
/// `adb push`'s source, the APK of `adb install`, `install` and `reinstall`, the scripts of
/// `replay` and `test`, and the `--image`, `--video` and `--steps-file` values of `display`,
/// `batch`, `replay` and `test`. Shell arguments, device paths and package names never are.
pub fn local_input(command: &str, args: &[String], i: usize) -> LocalInput {
    let prev = i.checked_sub(1).map(|p| args[p].as_str());
    match command {
        "adb" => match args.first().map(String::as_str) {
            Some("push") if i == 1 => LocalInput::Required,
            Some("install") if i > 0 && i + 1 == args.len() && !args[i].starts_with('-') => LocalInput::Required,
            _ => LocalInput::No,
        },
        // install <package> <path.apk>: the package name is never read as a file.
        "install" | "reinstall" if i == 1 => LocalInput::Required,
        "replay" | "test" | "display" | "batch" if matches!(prev, Some("--image" | "--video" | "--steps-file")) => {
            LocalInput::IfFile
        }
        "replay" | "test" if !args[i].starts_with('-') => LocalInput::IfFile,
        _ => LocalInput::No,
    }
}

/// Reads the local files `command`'s arguments name (see [`local_input`]) and replaces each such
/// argument with `attachment:<name>`. An argument that must be a local file and isn't, an `.aab`
/// where an APK is needed, or files past the limits are refused before anything is sent.
pub fn attach_local_files(command: &str, args: &mut [String]) -> Result<Vec<Attachment>, AttachmentError> {
    let mut set = AttachmentSet::new();
    for i in 0..args.len() {
        let arg = args[i].clone();
        let path = Path::new(&arg);
        match local_input(command, args, i) {
            LocalInput::No => continue,
            LocalInput::IfFile if !path.is_file() => continue,
            LocalInput::Required if !path.is_file() => {
                let found = match std::fs::metadata(path) {
                    Err(_) => Found::Nothing,
                    Ok(m) if m.is_dir() => Found::Directory,
                    Ok(_) => Found::NotRegular,
                };
                return Err(AttachmentError::NotAFile { path: arg, found });
            }
            LocalInput::Required
                if !(command == "adb" && args[0] == "push") && arg.to_ascii_lowercase().ends_with(".aab") =>
            {
                return Err(AttachmentError::AppBundle { path: arg });
            }
            LocalInput::IfFile | LocalInput::Required => {}
        }
        args[i] = set.add_path(path).map_err(|e| with_path(e, &arg))?;
    }
    Ok(set.into_vec())
}

/// The error as the caller typed the path, not as `Path::display` shows it.
fn with_path(e: AttachmentError, typed: &str) -> AttachmentError {
    let path = typed.to_owned();
    match e {
        AttachmentError::NotAFile { found, .. } => AttachmentError::NotAFile { path, found },
        AttachmentError::TooLarge { size, already, .. } => AttachmentError::TooLarge { path, size, already },
        AttachmentError::TooMany { .. } => AttachmentError::TooMany { path },
        AttachmentError::AppBundle { .. } => AttachmentError::AppBundle { path },
        AttachmentError::Unreadable { error, .. } => AttachmentError::Unreadable { path, error },
    }
}

/// The content type Extend sends for a file name, by its extension.
pub fn content_type_for(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    match lower.rsplit('.').next().unwrap_or_default() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "apk" => "application/vnd.android.package-archive",
        "json" => "application/json",
        "ad" | "txt" | "yaml" | "yml" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Standard, padded base64 (RFC 4648 §4), as [`Attachment::content_base64`] carries.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("extend-attachments-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn base64_matches_rfc4648() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(plain.as_bytes()), encoded);
            assert_eq!(Attachment::from_bytes("x", plain.as_bytes()).size(), plain.len() as u64);
        }
        assert_eq!(base64_encode(&[0xfb, 0xff, 0xbf]), "+/+/");
    }

    #[test]
    fn from_path_names_and_types_the_file() {
        let dir = scratch("from-path");
        let p = dir.join("shot.PNG");
        std::fs::write(&p, b"png bytes").unwrap();
        let a = Attachment::from_path(&p).unwrap();
        assert_eq!((a.name.as_str(), a.content_type.as_str()), ("shot.PNG", "image/png"));
        assert_eq!(a.content_base64, base64_encode(b"png bytes"));
        assert_eq!(a.size(), 9);
        assert_eq!(
            Attachment::from_path(dir.join("missing.ad")).unwrap_err(),
            AttachmentError::NotAFile {
                path: dir.join("missing.ad").display().to_string(),
                found: Found::Nothing
            }
        );
        assert!(matches!(
            Attachment::from_path(&dir).unwrap_err(),
            AttachmentError::NotAFile {
                found: Found::Directory,
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn oversized_files_are_refused_before_reading() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("oversized");
        let big = dir.join("system.img");
        std::fs::File::create(&big).unwrap().set_len(4 << 30).unwrap(); // sparse
        // Unreadable, so any attempt to read it would fail with "Permission denied" instead.
        std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o000)).unwrap();
        let e = Attachment::from_path(&big).unwrap_err();
        assert!(
            matches!(e, AttachmentError::TooLarge { size, already: 0, .. } if size == 4 << 30),
            "{e:?}"
        );
        assert!(e.to_string().contains("at most 8388608 bytes (8 MiB)"), "{e}");
        std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o600)).unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_set_keeps_to_eight_files_and_eight_mib() {
        let dir = scratch("limits");
        let five = dir.join("five.ad");
        std::fs::write(&five, vec![b'x'; 5 << 20]).unwrap();
        let mut set = AttachmentSet::new();
        assert_eq!(set.add_path(&five).unwrap(), "attachment:five.ad");
        // 5 MiB more would pass 8 MiB in total.
        let e = set.add_path(&five).unwrap_err();
        assert_eq!(
            e,
            AttachmentError::TooLarge {
                path: five.display().to_string(),
                size: 5 << 20,
                already: 5 << 20
            }
        );
        let exact = dir.join("exact.ad");
        std::fs::write(&exact, vec![b'x'; 8 << 20]).unwrap();
        assert!(
            AttachmentSet::new().add_path(&exact).is_ok(),
            "exactly 8 MiB is allowed"
        );
        let over = dir.join("over.ad");
        std::fs::write(&over, vec![b'x'; (8 << 20) + 1]).unwrap();
        assert!(matches!(
            AttachmentSet::new().add_path(&over),
            Err(AttachmentError::TooLarge { .. })
        ));

        let small = dir.join("flow.ad");
        std::fs::write(&small, b"open settings").unwrap();
        let mut set = AttachmentSet::new();
        let names: Vec<String> = (0..MAX_ATTACHMENTS).map(|_| set.add_path(&small).unwrap()).collect();
        assert_eq!(names[0], "attachment:flow.ad");
        assert_eq!(names[1], "attachment:flow-2.ad");
        assert_eq!(names[7], "attachment:flow-8.ad");
        assert!(matches!(set.add_path(&small), Err(AttachmentError::TooMany { .. })));
        assert_eq!((set.len(), set.total_bytes()), (8, 8 * 13));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_local_inputs_are_read_as_files() {
        use LocalInput::*;
        let at = |name: &str, items: &[&str], i: usize| local_input(name, &strings(items), i);
        assert_eq!(at("adb", &["push", "file.bin", "/sdcard/file.bin"], 1), Required);
        assert_eq!(at("adb", &["push", "file.bin", "/sdcard/file.bin"], 2), No);
        assert_eq!(at("adb", &["install", "-r", "app.apk"], 2), Required);
        assert_eq!(at("adb", &["shell", "cat", "file.bin"], 2), No);
        assert_eq!(at("adb", &["shell", "tool", "--image", "a.png"], 3), No);
        assert_eq!(at("install", &["com.example.app", "app.apk"], 0), No);
        assert_eq!(at("install", &["com.example.app", "app.apk"], 1), Required);
        assert_eq!(at("display", &["show", "--image", "https://x/a.png"], 2), IfFile);
        assert_eq!(at("replay", &["flow.ad"], 0), IfFile);
    }

    #[test]
    fn attach_local_files_rewrites_arguments() {
        let dir = scratch("attach");
        let apk = dir.join("app.apk");
        std::fs::write(&apk, b"PK").unwrap();
        let apk = apk.display().to_string();
        let mut args = strings(&["com.example.app", &apk]);
        let sent = attach_local_files("install", &mut args).unwrap();
        assert_eq!(args, strings(&["com.example.app", "attachment:app.apk"]));
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].content_type, "application/vnd.android.package-archive");

        let mut args = strings(&["com.example.app", "./nope.apk"]);
        assert_eq!(
            attach_local_files("install", &mut args).unwrap_err(),
            AttachmentError::NotAFile {
                path: "./nope.apk".into(),
                found: Found::Nothing
            }
        );
        let bundle = dir.join("app.aab");
        std::fs::write(&bundle, b"PK").unwrap();
        let mut args = strings(&["com.example.app", &bundle.display().to_string()]);
        assert!(matches!(
            attach_local_files("install", &mut args),
            Err(AttachmentError::AppBundle { .. })
        ));
        // A link where a file may go is the device's.
        let mut args = strings(&["show", "--image", "https://example.com/a.png"]);
        assert!(attach_local_files("display", &mut args).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
