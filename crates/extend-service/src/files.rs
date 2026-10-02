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
    /// Stable device-issued upload identity, retained across provider retries.
    pub operation_id: Uuid,
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
    api_url: String,
    web_url: String,
    iam: DynIam,
}
impl BriefcaseFiles {
    pub fn new(api_url: String, web_url: String, iam: DynIam) -> Self {
        Self {
            api_url,
            web_url: web_url.trim_end_matches('/').to_owned(),
            iam,
        }
    }
    async fn access(
        &self,
        member: &Principal,
        endpoint: &str,
        sel: Option<&TestingSelection>,
    ) -> AppResult<crate::iam::OboProof> {
        self.iam
            .obo_proof(member, "briefcase", endpoint, serde_json::json!({}), "POST", &[], sel)
            .await
    }
    fn client(&self, access: &crate::iam::OboProof) -> AppResult<briefcase_client::Client> {
        let org = access.org_id.as_ref().ok_or_else(|| {
            AppError::new(
                ErrorCode::ServiceUnavailable,
                "Briefcase permission is missing its selected organization.",
            )
        })?;
        let mut base =
            url::Url::parse(&self.api_url).map_err(|_| AppError::invalid("Briefcase API URL is invalid."))?;
        if base.path() == "/" {
            base.set_path("/api/v1/");
        }
        let mut cfg = briefcase_client::Config::new(base.as_str(), org).map_err(provider_error)?;
        if let Some(secret) = &access.testing_app_secret {
            cfg = cfg.with_environment(briefcase_client::EnvironmentKey::new(secret.clone()).map_err(provider_error)?);
        }
        briefcase_client::Client::new_unchecked(cfg).map_err(provider_error)
    }
    fn app(&self) -> AppResult<briefcase_client::ApplicationId> {
        briefcase_client::ApplicationId::new(self.iam.app_id()).map_err(provider_error)
    }
}
fn token(access: &crate::iam::OboProof) -> AppResult<briefcase_client::OboProof> {
    briefcase_client::OboProof::new(access.access_proof.clone()).map_err(provider_error)
}
fn same_context(a: &crate::iam::OboProof, b: &crate::iam::OboProof) -> AppResult<()> {
    if a.org_id != b.org_id || a.actor != b.actor || a.testing_app_secret != b.testing_app_secret {
        return Err(AppError::new(ErrorCode::ConfirmationRequired,"Choose the same Briefcase account and organization for Extend's upload, commit, read, sharing and deletion permissions.").hint("Review Extend's Briefcase permissions in Settings and approve them together."));
    }
    Ok(())
}
fn provider_error(error: briefcase_client::Error) -> AppError {
    match error {
        briefcase_client::Error::Api(e) => briefcase_error(
            e.status,
            &serde_json::json!({"error":{"code":e.code,"request_id":e.request_id}}),
            "approved endpoint",
            "complete the file operation",
            "the selected account",
        ),
        _ => AppError::new(
            ErrorCode::ServiceUnavailable,
            "Briefcase could not complete the file operation. Retry with the same upload identity.",
        ),
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
        401 => AppError::new(ErrorCode::ServiceUnavailable, format!("{what}: it did not accept Extend's saved feature approval."))
            .hint("Open Extend Settings → Permissions and renew the Briefcase approval, then retry."),
        403 => AppError::new(ErrorCode::NoAccess, format!("{what}: {member} may not do this there, or Extend is not approved for {endpoint_id}."))
            .hint("Check the selected Briefcase account and organization in Extend Settings → Permissions; approve the required endpoints in IAM."),
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
        use briefcase_client::delegated::{
            DelegatedCommitUpload, DelegatedInvite, DelegatedReserveUpload, DelegatedUploadQuery, DelegatedUploadState,
        };
        let reserve = self.access(silicon, "briefcase.uploads.reserve", sel).await?;
        // Request all required write authority before staging bytes. An approval for another
        // provider account must not strand a reservation or share somebody else's namespace.
        let commit = self.access(silicon, "briefcase.uploads.commit", sel).await?;
        same_context(&reserve, &commit)?;
        let status_access = self.access(silicon, "briefcase.uploads.status", sel).await?;
        same_context(&reserve, &status_access)?;
        let client = self.client(&reserve)?;
        let app = self.app()?;
        let name = operation_name(file.name, file.operation_id);
        let manifest = DelegatedReserveUpload {
            operation_id: file.operation_id,
            parent_path: String::new(),
            name: name.clone(),
            content_type: file.content_type.to_owned(),
            size: file.bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&file.bytes)),
        }
        .prepare()
        .map_err(provider_error)?;
        let reservation = client
            .reserve_delegated_upload(&app, token(&reserve)?, &manifest)
            .await
            .map_err(provider_error)?;
        let upload_id = reservation.status.upload_id;
        let mut state = reservation.status;
        if state.state == DelegatedUploadState::Reserved {
            let cap = reservation.capability.ok_or_else(|| {
                AppError::new(
                    ErrorCode::ServiceUnavailable,
                    "Briefcase did not return an upload capability.",
                )
            })?;
            match client
                .transfer_delegated_upload(upload_id, cap, &briefcase_client::UploadSource::Bytes(file.bytes))
                .await
            {
                Ok(status) => state = status,
                Err(_) => {
                    // Reconcile an uncertain transfer; never blindly resend bytes or create a
                    // second file. Keep the original operation UUID throughout.
                    let query = DelegatedUploadQuery {
                        operation_id: file.operation_id,
                    }
                    .prepare()
                    .map_err(provider_error)?;
                    state = client
                        .delegated_upload_status(&app, token(&status_access)?, &query)
                        .await
                        .map_err(provider_error)?;
                }
            }
        }
        if state.state == DelegatedUploadState::Staged {
            let manifest = DelegatedCommitUpload {
                operation_id: file.operation_id,
                upload_id,
            }
            .prepare()
            .map_err(provider_error)?;
            state = match client.commit_delegated_upload(&app, token(&commit)?, &manifest).await {
                Ok(status) => status,
                Err(_) => client
                    .delegated_upload_status(
                        &app,
                        token(&status_access)?,
                        &DelegatedUploadQuery {
                            operation_id: file.operation_id,
                        }
                        .prepare()
                        .map_err(provider_error)?,
                    )
                    .await
                    .map_err(provider_error)?,
            };
        }
        let file_id=state.published_entry_id.filter(|_|state.state==DelegatedUploadState::Committed).ok_or_else(||AppError::new(ErrorCode::ServiceUnavailable,"Briefcase has not published this upload yet. Retry the same upload; do not create another reservation."))?;
        let actor = reserve.actor.as_deref().ok_or_else(|| {
            AppError::new(
                ErrorCode::ServiceUnavailable,
                "Briefcase approval has no selected account.",
            )
        })?;
        let org = reserve.org_id.as_deref().unwrap_or_default();
        let mut url =
            url::Url::parse(&self.web_url).map_err(|_| AppError::invalid("Briefcase website URL is invalid."))?;
        url.path_segments_mut()
            .map_err(|_| AppError::invalid("Briefcase website URL is invalid."))?
            .extend(["org", org, "apps", self.iam.app_id(), "private", actor, &name]);
        let share = async {
            let invite_access = self.access(silicon, "briefcase.invitations.create", sel).await?;
            same_context(&reserve, &invite_access)?;
            let request = DelegatedInvite {
                operation_id: trash_operation_id(file.operation_id),
                entry_id: file_id,
                invitation: briefcase_client::Invite {
                    principal: briefcase_client::Recipient::Carbon(file.owner_carbon.to_owned()),
                    access: vec![
                        briefcase_client::AccessRight::Read,
                        briefcase_client::AccessRight::Update,
                    ],
                    inherit: true,
                    expires_in_minutes: None,
                },
            };
            let manifest = briefcase_client::DelegatedManifest::new(&request).map_err(provider_error)?;
            client
                .invite_on_behalf_of(&app, token(&invite_access)?, &manifest)
                .await
                .map_err(provider_error)?;
            AppResult::Ok(())
        }
        .await;
        let (shared_with, share_error) = match share {
            Ok(()) => (Some(file.owner_carbon.to_owned()), None),
            Err(e) => (None, Some(format!("{} {}", e.0.message, e.0.hint.unwrap_or_default()))),
        };
        Ok(Stored {
            file_id,
            url: url.to_string(),
            shared_with,
            share_error,
        })
    }
    async fn destroy(&self, silicon: &Principal, file_id: Uuid, sel: Option<&TestingSelection>) -> AppResult<()> {
        let access = self.access(silicon, "briefcase.entries.trash", sel).await?;
        let request = briefcase_client::DelegatedTrashEntry {
            operation_id: trash_operation_id(file_id),
            entry_id: file_id,
        }
        .prepare()
        .map_err(provider_error)?;
        match self
            .client(&access)?
            .trash_entry_on_behalf_of(&self.app()?, token(&access)?, &request)
            .await
            .map_err(provider_error)
        {
            Ok(()) => Ok(()),
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
        let access = self.access(member, "briefcase.files.read", sel).await?;
        let request = briefcase_client::DelegatedReadFile {
            entry_id: file_id,
            range: None,
            download: false,
        }
        .prepare()
        .map_err(provider_error)?;
        let mut response = self
            .client(&access)?
            .read_file_on_behalf_of(&self.app()?, token(&access)?, &request)
            .await
            .map_err(provider_error)?;
        let content_type = response.content_type().unwrap_or("application/octet-stream").to_owned();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(provider_error)? {
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
