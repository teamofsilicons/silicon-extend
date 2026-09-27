//! Where files a command produces end up (TECHNICAL.md section 6).
//!
//! [`BriefcaseFiles`] stores each file in Briefcase on the Silicon's behalf through Briefcase's OBO
//! endpoints and shares it with the device's owner. [`LocalFiles`] keeps files on disk for
//! development and tests, refused in production.
//!
//! What Briefcase's OBO surface allows, as verified against a real Briefcase (e2e/real-iam):
//! - Its delegated upload takes no self-destruct time and there is no delegated "make permanent"
//!   (TECHNICAL.md open questions 1–2). Extend records the self-destruct time, deletes the file
//!   itself when it passes (through Briefcase's delegated trash), and treats "keep" as cancelling
//!   that deletion.
//! - A name that already exists in the folder publishes a new *version* of that file instead of a
//!   new file. Every stored file therefore gets its own name ([`unique_name`]); otherwise two
//!   screenshots would share one Briefcase entry and trashing one would trash both.
//! - `write` (create) can only be granted on a folder. The device owner gets `read` and `update`
//!   on each file; never `delete`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use extend_protocol::ErrorCode;
use rand::Rng as _;
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use tokio::io::AsyncReadExt as _;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::iam::{DynIam, Principal, TestingSelection};

#[derive(Debug, Clone)]
pub struct Stored {
    pub file_id: Uuid,
    pub url: String,
    pub shared_with: Option<String>,
    /// Why the file could not be shared with the device's Carbon (it is stored either way), said
    /// so the Silicon can be told: what happened, why, and what to do.
    pub share_error: Option<String>,
}

pub struct NewFile<'a> {
    pub name: &'a str,
    pub content_type: &'a str,
    pub bytes: Vec<u8>,
    pub owner_carbon: &'a str,
}

#[async_trait]
pub trait FileStore: Send + Sync {
    async fn store(&self, silicon: &Principal, file: NewFile<'_>, sel: Option<&TestingSelection>) -> AppResult<Stored>;
    /// Deletes a file whose self-destruct time has passed.
    async fn destroy(&self, silicon: &Principal, file_id: Uuid, sel: Option<&TestingSelection>) -> AppResult<()>;
    /// A file's bytes and media type, read as `member` (the Silicon that made it, or the device
    /// owner it was shared with).
    async fn read(
        &self,
        member: &Principal,
        file_id: Uuid,
        sel: Option<&TestingSelection>,
    ) -> AppResult<(Vec<u8>, String)> {
        self.read_bounded(member, file_id, sel, usize::MAX).await
    }
    /// Refuses oversized responses while reading, before allocating their entire contents.
    async fn read_bounded(
        &self,
        member: &Principal,
        file_id: Uuid,
        sel: Option<&TestingSelection>,
        max_bytes: usize,
    ) -> AppResult<(Vec<u8>, String)>;
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
    api_url: String,
    web_url: String,
    iam: DynIam,
}

/// One delegated call's request.
struct Delegated<'a> {
    endpoint_id: &'a str,
    path: &'a str,
    metadata: serde_json::Value,
    body: Vec<u8>,
    content_type: &'a str,
    /// What the call does, for error messages ("store screenshot.png").
    doing: String,
}

impl BriefcaseFiles {
    pub fn new(api_url: String, web_url: String, iam: DynIam) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_url: api_url.trim_end_matches('/').to_owned(),
            web_url: web_url.trim_end_matches('/').to_owned(),
            iam,
        }
    }

    /// Sends one proof-bound request to Briefcase on `member`'s behalf. A proof is single-use, so
    /// this never retries by itself.
    async fn delegated(
        &self,
        member: &Principal,
        call: Delegated<'_>,
        sel: Option<&TestingSelection>,
    ) -> AppResult<reqwest::Response> {
        if member.token.is_empty() {
            return Err(AppError::new(
                ErrorCode::NotSignedIn,
                format!(
                    "Extend could not {} in Briefcase: it holds no signed-in session for {} to act with.",
                    call.doing,
                    member.id()
                ),
            )
            .hint(format!("It happens the next time {} uses Extend.", member.id())));
        }
        let proof = self
            .iam
            .obo_proof(
                member,
                "briefcase",
                call.endpoint_id,
                call.metadata,
                "POST",
                &call.body,
                sel,
            )
            .await?;
        let mut req = self
            .http
            .post(format!("{}{}", self.api_url, call.path))
            .header("X-App-ID", self.iam.app_id())
            .header("X-IAM-OBO-Access-Proof", &proof.access_proof)
            .header("Content-Type", call.content_type)
            .body(call.body);
        if let Some(team) = &member.team {
            req = req.header("X-Org-ID", team);
        }
        if let Some(secret) = &proof.testing_app_secret {
            req = req.header("X-Briefcase-App-Secret", secret);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| AppError::unavailable("Briefcase", format!("could not {}: {e}", call.doing)))?;
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status().as_u16();
        let json: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        Err(briefcase_error(
            status,
            &json,
            call.endpoint_id,
            &call.doing,
            member.id(),
        ))
    }
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
        401 => AppError::new(ErrorCode::ServiceUnavailable, format!("{what}: it did not accept the single-use IAM proof Extend sent."))
            .hint("Retry the command; each try gets a new proof. If it keeps failing, report it with `extend report`."),
        403 => AppError::new(ErrorCode::NoAccess, format!("{what}: {member} may not do this there, or Extend is not approved for {endpoint_id}."))
            .hint("A Team admin can check Extend's Briefcase approval in Honeycomb; the Silicon may need to sign in to Extend again to approve it."),
        404 => AppError::new(ErrorCode::FileNotFound, format!("{what}: the file or folder does not exist or {member} cannot see it.")),
        413 | 507 => AppError::new(ErrorCode::PayloadTooLarge, format!("{what}: the file is too large or the Team's Briefcase storage is full."))
            .hint("Free space in Briefcase, or ask a Team admin to raise the Team's storage allowance."),
        // Briefcase's delegated invitation only accepts members it has already seen (from their own
        // Briefcase sign-in or an IAM webhook); it does not look the recipient up in IAM.
        422 if code == "invalid_principal" => AppError::new(
            ErrorCode::NoAccess,
            format!("{what}: Briefcase does not know the recipient as a current member of the Team yet."),
        )
        .hint("The file is stored. Briefcase learns a Carbon once they sign in to Briefcase; files made after that are shared with them."),
        422 | 400 => AppError::new(ErrorCode::ServiceUnavailable, format!("{what}: Briefcase considers the request invalid."))
            .hint("This is a mismatch between Extend and Briefcase; report it with `extend report`."),
        429 => AppError::new(ErrorCode::RateLimited, format!("{what}: Briefcase is rate limiting.")).hint("Retry in a minute."),
        _ => AppError::unavailable("Briefcase", what),
    }
}

#[async_trait]
impl FileStore for BriefcaseFiles {
    async fn store(&self, silicon: &Principal, file: NewFile<'_>, sel: Option<&TestingSelection>) -> AppResult<Stored> {
        let name = unique_name(file.name, OffsetDateTime::now_utc(), rand::rng().random());
        let resp = self
            .delegated(
                silicon,
                Delegated {
                    endpoint_id: "briefcase.files.create",
                    path: "/api/v1/obo/files",
                    metadata: serde_json::json!({"path": "", "name": name, "content_type": file.content_type}),
                    body: file.bytes,
                    content_type: "application/octet-stream",
                    doing: format!("store {name}"),
                },
                sel,
            )
            .await?;
        let entry: serde_json::Value = resp.json().await.map_err(|e| {
            AppError::unavailable("Briefcase", format!("its answer to storing {name} was unreadable: {e}"))
        })?;
        let file_id = entry
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                AppError::unavailable("Briefcase", format!("its answer to storing {name} had no entry id"))
            })?;
        let team = silicon.team.clone().unwrap_or_default();
        let url = entry
            .get("permanent_url")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "{}/org/{team}/apps/{}/private/{}/{name}",
                    self.web_url,
                    self.iam.app_id(),
                    silicon.id()
                )
            });
        // Share with the device's owner: read and update, never delete (Briefcase's critical
        // `briefcase.invitations.create`, which Extend must be approved for in Honeycomb).
        let invite = serde_json::json!({
            "operation_id": Uuid::new_v4(),
            "entry_id": file_id,
            "invitation": {"principal": {"type": "carbon", "id": file.owner_carbon}, "access": OWNER_ACCESS, "inherit": true}
        });
        let body = serde_json::to_vec(&invite).map_err(AppError::internal)?;
        let call = Delegated {
            endpoint_id: "briefcase.invitations.create",
            path: "/api/v1/obo/invitations",
            metadata: serde_json::json!({}),
            body,
            content_type: "application/json",
            doing: format!("share {name} with {}", file.owner_carbon),
        };
        let (shared_with, share_error) = match self.delegated(silicon, call, sel).await {
            Ok(_) => (Some(file.owner_carbon.to_owned()), None),
            Err(e) => {
                // The file is stored either way; the owner just can't open it in Briefcase yet.
                tracing::warn!(file_id = %file_id, error = %e.0.message, hint = ?e.0.hint, "sharing an Extend file with the device owner failed");
                let why = match &e.0.hint {
                    Some(h) => format!("{} {h}", e.0.message),
                    None => e.0.message.clone(),
                };
                (None, Some(why))
            }
        };
        Ok(Stored {
            file_id,
            url,
            shared_with,
            share_error,
        })
    }

    async fn destroy(&self, silicon: &Principal, file_id: Uuid, sel: Option<&TestingSelection>) -> AppResult<()> {
        let body =
            serde_json::to_vec(&serde_json::json!({"operation_id": trash_operation_id(file_id), "entry_id": file_id}))
                .map_err(AppError::internal)?;
        let call = Delegated {
            endpoint_id: "briefcase.entries.trash",
            path: "/api/v1/obo/entries/trash",
            metadata: serde_json::json!({}),
            body,
            content_type: "application/json",
            doing: format!("delete file {file_id} after its self-destruct time"),
        };
        match self.delegated(silicon, call, sel).await {
            Ok(_) => Ok(()),
            // Already gone (trashed elsewhere, or by an earlier try whose answer was lost).
            Err(e) if e.code() == ErrorCode::FileNotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn read_bounded(
        &self,
        member: &Principal,
        file_id: Uuid,
        sel: Option<&TestingSelection>,
        max_bytes: usize,
    ) -> AppResult<(Vec<u8>, String)> {
        let body = serde_json::to_vec(&serde_json::json!({"entry_id": file_id})).map_err(AppError::internal)?;
        let call = Delegated {
            endpoint_id: "briefcase.files.read",
            path: "/api/v1/obo/files/read",
            metadata: serde_json::json!({}),
            body,
            content_type: "application/json",
            doing: format!("read file {file_id}"),
        };
        let mut resp = self.delegated(member, call, sel).await?;
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        if resp.content_length().is_some_and(|n| n > max_bytes as u64) {
            return Err(too_large(max_bytes));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| AppError::unavailable("Briefcase", format!("reading file {file_id} broke off: {e}")))?
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
    async fn store(
        &self,
        _silicon: &Principal,
        file: NewFile<'_>,
        _sel: Option<&TestingSelection>,
    ) -> AppResult<Stored> {
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
            shared_with: Some(file.owner_carbon.to_owned()),
            share_error: None,
        })
    }

    async fn destroy(&self, _silicon: &Principal, file_id: Uuid, _sel: Option<&TestingSelection>) -> AppResult<()> {
        let _ = tokio::fs::remove_file(self.dir.join(file_id.to_string())).await;
        let _ = tokio::fs::remove_file(self.dir.join(format!("{file_id}.type"))).await;
        Ok(())
    }

    async fn read_bounded(
        &self,
        _member: &Principal,
        file_id: Uuid,
        _sel: Option<&TestingSelection>,
        max_bytes: usize,
    ) -> AppResult<(Vec<u8>, String)> {
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
                && e.0.hint.as_deref().unwrap_or_default().contains("sign in to Briefcase")
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
