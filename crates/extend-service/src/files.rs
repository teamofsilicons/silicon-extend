//! Where files a command produces end up (TECHNICAL.md section 6).
//!
//! [`BriefcaseFiles`] stores each file in the Silicon's own Briefcase (its app folder for Extend)
//! through Briefcase's delegated routes (`/api/v1/obo/…`), with a Silicon Accounts User
//! verification proof for the Silicon (`Authorization: Proof sap_…`), and shares it with the
//! Carbon who paired the device. [`LocalFiles`] keeps files on disk for development and tests,
//! refused in production.
//!
//! What Briefcase's delegated routes allow:
//! - Its delegated upload takes no self-destruct time and there is no delegated "make permanent"
//!   (TECHNICAL.md open questions 1–2). Extend records the self-destruct time, deletes the file
//!   itself when it passes (through the delegated trash, with the proof it keeps for the Silicon),
//!   and treats "keep" as cancelling that deletion.
//! - A name that already exists in the folder publishes a new *version* of that file instead of a
//!   new file. Every stored file therefore gets its own name ([`unique_name`]); otherwise two
//!   screenshots would share one Briefcase entry and trashing one would trash both.
//! - `write` (create) can only be granted on a folder. The device owner gets `read` and `update`
//!   on each file; never `delete`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use extend_protocol::ErrorCode;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use tokio::io::AsyncReadExt as _;
use uuid::Uuid;

use crate::accounts::Principal;
use crate::error::{AppError, AppResult};
use crate::proofs::{BRIEFCASE, BRIEFCASE_READ_SCOPES, BRIEFCASE_WRITE_SCOPES, ProofStore};

#[derive(Debug, Clone)]
pub struct Stored {
    pub file_id: Uuid,
    pub url: String,
    /// The uuid of the Carbon it is shared with.
    pub shared_with: Option<String>,
    /// Why the file could not be shared with the device's Carbon (it is stored either way), said
    /// so the Silicon can be told: what happened, why, and what to do.
    pub share_error: Option<String>,
}

/// The Carbon who paired the device a file was made on: who it is shared with.
#[derive(Debug, Clone)]
pub struct Recipient<'a> {
    pub uuid: &'a str,
    /// Their current public id (`c:ada`): Briefcase looks invitees up by id.
    pub id: &'a str,
}

pub struct NewFile<'a> {
    /// Stable device-issued upload identity, retained across provider retries.
    pub operation_id: Uuid,
    pub name: &'a str,
    pub content_type: &'a str,
    pub bytes: Vec<u8>,
    pub owner_carbon: Recipient<'a>,
}

#[async_trait]
pub trait FileStore: Send + Sync {
    /// Stores a file a Silicon's command made, as that Silicon (its sign-in is live: it just ran
    /// the command), and shares it with the Carbon who paired the device.
    async fn store(&self, silicon: &Principal, file: NewFile<'_>) -> AppResult<Stored>;
    /// Deletes a file whose self-destruct time has passed, as the Silicon that made it
    /// (`creator`, a uuid), with the proof Extend keeps for it.
    async fn destroy(&self, creator: &str, file_id: Uuid) -> AppResult<()>;
    /// A file's bytes and media type, read as `reader` (the Silicon that made it, the Carbon it was
    /// shared with, or the Silicon's custodian), whose sign-in is live.
    async fn read(&self, reader: &Principal, file_id: Uuid) -> AppResult<(Vec<u8>, String)> {
        self.read_bounded(reader, file_id, usize::MAX).await
    }
    /// Refuses oversized responses while reading, before allocating their entire contents.
    async fn read_bounded(&self, reader: &Principal, file_id: Uuid, max_bytes: usize) -> AppResult<(Vec<u8>, String)>;
    /// Local files only: the bytes behind `/dev/files/{id}`.
    async fn read_local(&self, _file_id: Uuid) -> Option<(Vec<u8>, String)> {
        None
    }
}

pub type DynFiles = Arc<dyn FileStore>;

/// The access the device owner gets on each file: Briefcase grants `write` (create) only on
/// folders, and `delete` is never shared.
pub const OWNER_ACCESS: [&str; 2] = ["read", "update"];

/// A Briefcase name of its own for every stored file: the device's name with the UTC time and a
/// random suffix before the extension (`screenshot.png` → `screenshot-20260926-151850-9f3a61c2.png`).
pub fn unique_name(name: &str, at: OffsetDateTime, suffix: u32) -> String {
    let clean: String = name
        .trim()
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = if clean.is_empty() || clean == "." || clean == ".." {
        "file".to_owned()
    } else {
        clean
    };
    let (stem, ext) = match clean.rfind('.') {
        Some(i) if i > 0 && clean.len() - i <= 16 => (&clean[..i], &clean[i..]),
        _ => (clean.as_str(), ""),
    };
    // Briefcase allows 255 bytes; keep well inside it on a character boundary.
    let mut end = stem.len().min(150);
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    let at = at.to_offset(time::UtcOffset::UTC);
    format!(
        "{}-{:04}{:02}{:02}-{:02}{:02}{:02}-{suffix:08x}{ext}",
        &stem[..end],
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// A stable unique file name for a device upload, including after a service restart.
fn operation_name(name: &str, id: Uuid) -> String {
    let clean: String = name
        .trim()
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = if clean.is_empty() || clean == "." || clean == ".." {
        "file"
    } else {
        &clean
    };
    let (stem, extension) = match clean.rfind('.') {
        Some(i) if i > 0 && clean.len() - i <= 16 => (&clean[..i], &clean[i..]),
        _ => (clean, ""),
    };
    let mut end = stem.len().min(150);
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}-{id}{extension}", &stem[..end])
}

/// The logical operation id for trashing a file: the same for every retry, so Briefcase treats a
/// retry after an uncertain answer as the same deletion.
pub fn trash_operation_id(file_id: Uuid) -> Uuid {
    let digest = Sha256::digest(format!("silicon-extend:briefcase.entries.trash:{file_id}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

// ───────────────────────────── Briefcase ─────────────────────────────

pub struct BriefcaseFiles {
    http: reqwest::Client,
    /// Briefcase's API origin (`EXTEND_BRIEFCASE_URL`).
    api_url: String,
    /// Briefcase's website (`EXTEND_BRIEFCASE_WEB_URL`), for the file links Extend shows.
    web_url: String,
    app_id: String,
    proofs: Arc<ProofStore>,
}

impl BriefcaseFiles {
    pub fn new(api_url: String, web_url: String, app_id: String, proofs: Arc<ProofStore>) -> Self {
        let api = api_url.trim().trim_end_matches('/');
        let api = api.strip_suffix("/api/v1").unwrap_or(api).to_owned();
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .user_agent(concat!("silicon-extend/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default(),
            api_url: api,
            web_url: web_url.trim().trim_end_matches('/').to_owned(),
            app_id,
            proofs,
        }
    }

    /// One delegated JSON call: `POST /api/v1/obo/{path}` with `Authorization: Proof …`.
    async fn call(
        &self,
        proof: &str,
        path: &str,
        body: &Value,
        doing: &str,
        member: &str,
    ) -> AppResult<reqwest::Response> {
        let resp = self
            .http
            .post(format!("{}/api/v1/obo/{path}", self.api_url))
            .header("authorization", format!("Proof {proof}"))
            .json(body)
            .send()
            .await
            .map_err(|e| AppError::unavailable("Briefcase", format!("{doing}: {e}")))?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        Err(briefcase_error(
            status,
            &body,
            &format!("/api/v1/obo/{path}"),
            doing,
            member,
        ))
    }

    async fn call_json(&self, proof: &str, path: &str, body: &Value, doing: &str, member: &str) -> AppResult<Value> {
        let resp = self.call(proof, path, body, doing, member).await?;
        resp.json()
            .await
            .map_err(|e| AppError::unavailable("Briefcase", format!("{doing}: an unexpected answer ({e})")))
    }

    async fn status(&self, proof: &str, operation_id: Uuid, member: &str) -> AppResult<Value> {
        self.call_json(
            proof,
            "uploads/status",
            &json!({"operation_id": operation_id}),
            "check an upload",
            member,
        )
        .await
    }

    fn file_url(&self, owner_id: &str, name: &str) -> String {
        match url::Url::parse(&self.web_url) {
            Ok(mut url) => {
                if let Ok(mut segments) = url.path_segments_mut() {
                    segments
                        .pop_if_empty()
                        .extend([owner_id, "apps", self.app_id.as_str(), name]);
                }
                url.to_string()
            }
            Err(_) => format!("{}/{owner_id}/apps/{}/{name}", self.web_url, self.app_id),
        }
    }
}

fn state_of(v: &Value) -> &str {
    v.get("state").and_then(Value::as_str).unwrap_or("")
}

fn entry_of(v: &Value) -> Option<Uuid> {
    v.get("published_entry_id")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
}

/// Turns a Briefcase refusal into an error that says what happened, why, and what to do.
pub fn briefcase_error(
    status: u16,
    body: &serde_json::Value,
    endpoint_id: &str,
    doing: &str,
    member: &str,
) -> AppError {
    let code = body
        .pointer("/error/code")
        .and_then(|v| v.as_str())
        .unwrap_or("no code");
    let request = body
        .pointer("/error/request_id")
        .and_then(|v| v.as_str())
        .map(|r| format!(", Briefcase request {r}"))
        .unwrap_or_default();
    let what = format!("Briefcase refused to {doing} for {member} ({endpoint_id} answered {status} {code}{request})");
    match status {
        401 => AppError::new(
            ErrorCode::ServiceUnavailable,
            format!("{what}: it did not accept the Silicon Accounts proof Extend sent."),
        )
        .hint("Extend gets a new proof on the next try; if it keeps failing, Briefcase may not accept proofs from Extend yet. Report it with `extend report`."),
        403 => AppError::new(
            ErrorCode::NoAccess,
            format!("{what}: {member} may not do this there, or Briefcase doesn't accept Extend for {endpoint_id}."),
        )
        .hint("Briefcase decides which apps may act for an account and where; ask a Briefcase Carbon to allow Extend's scopes."),
        404 => AppError::new(
            ErrorCode::FileNotFound,
            format!("{what}: the file or folder does not exist or {member} cannot see it."),
        ),
        413 | 507 => AppError::new(
            ErrorCode::PayloadTooLarge,
            format!("{what}: the file is too large or the account's Briefcase storage is full."),
        )
        .hint("Free space in Briefcase, then run the command again."),
        422 if code == "invalid_principal" || code == "unknown_recipient" => AppError::new(
            ErrorCode::NoAccess,
            format!("{what}: Briefcase does not know the recipient."),
        )
        .hint("The file is stored; share it from Briefcase by hand if the Carbon needs it."),
        422 | 400 => AppError::new(
            ErrorCode::ServiceUnavailable,
            format!("{what}: Briefcase considers the request invalid."),
        )
        .hint("This is a mismatch between Extend and Briefcase; report it with `extend report`."),
        429 => AppError::new(ErrorCode::RateLimited, format!("{what}: Briefcase is rate limiting.")).hint("Retry in a minute."),
        _ => AppError::unavailable("Briefcase", what),
    }
}

#[async_trait]
impl FileStore for BriefcaseFiles {
    async fn store(&self, silicon: &Principal, file: NewFile<'_>) -> AppResult<Stored> {
        let who = silicon.public_id();
        let proof = self.proofs.for_user(silicon, BRIEFCASE, BRIEFCASE_WRITE_SCOPES).await?;
        let name = operation_name(file.name, file.operation_id);
        let size = file.bytes.len();
        let reservation = self
            .call_json(
                &proof,
                "uploads/reserve",
                &json!({
                    "operation_id": file.operation_id,
                    "parent_path": "",
                    "name": name,
                    "content_type": file.content_type,
                    "size": size,
                    "sha256": format!("{:x}", Sha256::digest(&file.bytes)),
                }),
                &format!("reserve the upload of {}", file.name),
                who,
            )
            .await?;
        let upload_id: Uuid = reservation
            .get("upload_id")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| AppError::unavailable("Briefcase", "the upload reservation has no upload_id"))?;
        let mut state = reservation.clone();
        if state_of(&state) == "reserved" {
            let capability = reservation
                .get("capability")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::unavailable("Briefcase", "the upload reservation has no capability"))?;
            let sent = self
                .http
                .put(format!("{}/api/v1/obo/uploads/{upload_id}/content", self.api_url))
                .header("x-briefcase-upload-capability", capability)
                .header("content-type", "application/octet-stream")
                .header("content-length", size)
                .body(file.bytes)
                .send()
                .await;
            state = match sent {
                Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
                // Reconcile an uncertain transfer; never blindly resend bytes or create a second
                // file. The operation id stays the same throughout.
                _ => self.status(&proof, file.operation_id, who).await?,
            };
            if state_of(&state).is_empty() {
                state = self.status(&proof, file.operation_id, who).await?;
            }
        }
        if state_of(&state) == "staged" {
            state = match self
                .call_json(
                    &proof,
                    "uploads/commit",
                    &json!({"operation_id": file.operation_id, "upload_id": upload_id}),
                    &format!("publish {}", file.name),
                    who,
                )
                .await
            {
                Ok(s) => s,
                Err(_) => self.status(&proof, file.operation_id, who).await?,
            };
        }
        let file_id = entry_of(&state)
            .filter(|_| state_of(&state) == "committed")
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::ServiceUnavailable,
                    format!(
                        "Briefcase has not published {} yet (its upload is {}).",
                        file.name,
                        if state_of(&state).is_empty() {
                            "in an unknown state"
                        } else {
                            state_of(&state)
                        }
                    ),
                )
                .hint("Run the command again; Extend never makes a second copy of the same upload.")
            })?;
        let url = self.file_url(who, &name);
        let share = self
            .call(
                &proof,
                "invitations",
                &json!({
                    "operation_id": trash_operation_id(file.operation_id),
                    "entry_id": file_id,
                    "invitation": {
                        "principal": {"type": "carbon", "id": file.owner_carbon.id},
                        "access": ["read", "update"],
                        "inherit": true,
                    },
                }),
                &format!("share {} with {}", file.name, file.owner_carbon.id),
                who,
            )
            .await;
        let (shared_with, share_error) = match share {
            Ok(_) => (Some(file.owner_carbon.uuid.to_owned()), None),
            Err(e) => (None, Some(format!("{} {}", e.0.message, e.0.hint.unwrap_or_default()))),
        };
        Ok(Stored {
            file_id,
            url,
            shared_with,
            share_error,
        })
    }

    async fn destroy(&self, creator: &str, file_id: Uuid) -> AppResult<()> {
        let Some(proof) = self.proofs.held(creator, BRIEFCASE, BRIEFCASE_WRITE_SCOPES).await? else {
            return Err(AppError::new(
                ErrorCode::ServiceUnavailable,
                format!(
                    "Extend holds no Briefcase proof for the Silicon that made file {file_id}, so it can't trash it yet."
                ),
            )
            .hint("It is trashed after the Silicon next uses Extend (Extend then gets a new proof)."));
        };
        match self
            .call(
                &proof,
                "entries/trash",
                &json!({"operation_id": trash_operation_id(file_id), "entry_id": file_id}),
                "trash a self-destructed file",
                creator,
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(e) if e.code() == ErrorCode::FileNotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn read_bounded(&self, reader: &Principal, file_id: Uuid, max_bytes: usize) -> AppResult<(Vec<u8>, String)> {
        let proof = self.proofs.for_user(reader, BRIEFCASE, BRIEFCASE_READ_SCOPES).await?;
        let mut response = self
            .call(
                &proof,
                "files/read",
                &json!({"entry_id": file_id, "range": null, "download": false}),
                "read a file",
                reader.public_id(),
            )
            .await?;
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| AppError::unavailable("Briefcase", format!("reading a file: {e}")))?
        {
            if chunk.len() > max_bytes.saturating_sub(bytes.len()) {
                return Err(too_large(max_bytes));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes, content_type))
    }
}

// ───────────────────────────── Local disk ─────────────────────────────

pub struct LocalFiles {
    dir: PathBuf,
    public_url: String,
}

impl LocalFiles {
    pub fn new(dir: &Path, public_url: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_owned(),
            public_url: public_url.trim_end_matches('/').to_owned(),
        })
    }
}

#[async_trait]
impl FileStore for LocalFiles {
    async fn store(&self, _silicon: &Principal, file: NewFile<'_>) -> AppResult<Stored> {
        let file_id = Uuid::now_v7();
        tokio::fs::write(self.dir.join(file_id.to_string()), &file.bytes)
            .await
            .map_err(AppError::internal)?;
        tokio::fs::write(self.dir.join(format!("{file_id}.type")), file.content_type)
            .await
            .map_err(AppError::internal)?;
        Ok(Stored {
            file_id,
            url: format!("{}/dev/files/{file_id}", self.public_url),
            shared_with: Some(file.owner_carbon.uuid.to_owned()),
            share_error: None,
        })
    }

    async fn destroy(&self, _creator: &str, file_id: Uuid) -> AppResult<()> {
        let _ = tokio::fs::remove_file(self.dir.join(file_id.to_string())).await;
        let _ = tokio::fs::remove_file(self.dir.join(format!("{file_id}.type"))).await;
        Ok(())
    }

    async fn read_bounded(&self, _reader: &Principal, file_id: Uuid, max_bytes: usize) -> AppResult<(Vec<u8>, String)> {
        let file = tokio::fs::File::open(self.dir.join(file_id.to_string()))
            .await
            .map_err(|_| not_found())?;
        if file.metadata().await.map_err(AppError::internal)?.len() > max_bytes as u64 {
            return Err(too_large(max_bytes));
        }
        let mut bytes = Vec::new();
        file.take((max_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .await
            .map_err(AppError::internal)?;
        if bytes.len() > max_bytes {
            return Err(too_large(max_bytes));
        }
        let ct = tokio::fs::read_to_string(self.dir.join(format!("{file_id}.type")))
            .await
            .unwrap_or_else(|_| "application/octet-stream".into());
        Ok((bytes, ct))
    }

    async fn read_local(&self, file_id: Uuid) -> Option<(Vec<u8>, String)> {
        let bytes = tokio::fs::read(self.dir.join(file_id.to_string())).await.ok()?;
        let ct = tokio::fs::read_to_string(self.dir.join(format!("{file_id}.type")))
            .await
            .unwrap_or_else(|_| "application/octet-stream".into());
        Some((bytes, ct))
    }
}

pub fn not_found() -> AppError {
    AppError::new(
        ErrorCode::FileNotFound,
        "The file does not exist, already self-destructed, or is not visible to you.",
    )
}

pub fn too_large(max_bytes: usize) -> AppError {
    AppError::invalid(format!("The file exceeds the remaining attachment limit ({max_bytes} bytes)."))
        .hint("Command attachments are limited to 8 files and 8 MiB in total. Use a smaller file or a directly accessible media URL.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_names_keep_the_name_and_extension() {
        let at = time::macros::datetime!(2026-09-26 15:18:50 UTC);
        assert_eq!(
            unique_name("screenshot.png", at, 0x9f3a61c2),
            "screenshot-20260926-151850-9f3a61c2.png"
        );
        assert_eq!(unique_name("recording", at, 1), "recording-20260926-151850-00000001");
        assert_eq!(
            unique_name("a/b\\c.tar.gz", at, 2),
            "a_b_c.tar-20260926-151850-00000002.gz"
        );
        assert_eq!(unique_name(" .. ", at, 3), "file-20260926-151850-00000003");
        assert_eq!(unique_name(".env", at, 4), ".env-20260926-151850-00000004");
        let long = "é".repeat(200) + ".png";
        let n = unique_name(&long, at, 5);
        assert!(n.len() <= 200 && n.ends_with("-00000005.png"), "{n}");
        assert_ne!(
            unique_name("screenshot.png", at, 6),
            unique_name("screenshot.png", at, 7)
        );
    }

    #[test]
    fn trash_operation_ids_are_stable_per_file() {
        let a = Uuid::now_v7();
        assert_eq!(trash_operation_id(a), trash_operation_id(a));
        assert_ne!(trash_operation_id(a), trash_operation_id(Uuid::now_v7()));
        assert!(!trash_operation_id(a).is_nil());
    }

    #[test]
    fn refusals_say_what_why_and_what_to_do() {
        let body = serde_json::json!({"error": {"code": "invalid_access", "message": "The request contains invalid data.", "request_id": "r1"}});
        let e = briefcase_error(
            422,
            &body,
            "briefcase.invitations.create",
            "share x.png with c:alice",
            "si:chef",
        );
        assert!(
            e.0.message.contains("share x.png with c:alice")
                && e.0.message.contains("invalid_access")
                && e.0.message.contains("r1")
        );
        assert!(e.0.hint.is_some());
        let unseen = serde_json::json!({"error": {"code": "invalid_principal"}});
        let e = briefcase_error(
            422,
            &unseen,
            "briefcase.invitations.create",
            "share x.png with c:alice",
            "si:chef",
        );
        assert!(
            e.0.message.contains("does not know the recipient")
                && e.0
                    .hint
                    .as_deref()
                    .unwrap_or_default()
                    .contains("share it from Briefcase")
        );
        assert_eq!(
            briefcase_error(
                404,
                &serde_json::Value::Null,
                "briefcase.entries.trash",
                "delete",
                "si:chef"
            )
            .code(),
            ErrorCode::FileNotFound
        );
        assert_eq!(
            briefcase_error(
                403,
                &serde_json::Value::Null,
                "briefcase.files.create",
                "store",
                "si:chef"
            )
            .code(),
            ErrorCode::NoAccess
        );
        assert_eq!(
            briefcase_error(
                503,
                &serde_json::Value::Null,
                "briefcase.files.create",
                "store",
                "si:chef"
            )
            .code(),
            ErrorCode::ServiceUnavailable
        );
    }
}
