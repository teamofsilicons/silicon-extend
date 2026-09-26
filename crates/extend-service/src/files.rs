//! Where files a command produces end up (TECHNICAL.md section 6).
//!
//! [`BriefcaseFiles`] stores each file in Briefcase on the Silicon's behalf through Briefcase's OBO
//! endpoints and shares it with the device's owner (read, write, update; never delete).
//! [`LocalFiles`] keeps files on disk for development and tests, refused in production.
//!
//! Known gaps in Briefcase's OBO surface today (TECHNICAL.md open questions 1–2): its delegated
//! upload takes no self-destruct time and there is no delegated "make permanent". Extend records
//! the self-destruct time it was asked for, deletes the file itself when it passes (through
//! Briefcase's delegated trash), and treats "keep" as cancelling that deletion.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use extend_protocol::ErrorCode;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::iam::{DynIam, Principal, TestingSelection};

#[derive(Debug, Clone)]
pub struct Stored {
    pub file_id: Uuid,
    pub url: String,
    pub shared_with: Option<String>,
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
    /// Local files only: the bytes behind `/dev/files/{id}`.
    async fn read_local(&self, _file_id: Uuid) -> Option<(Vec<u8>, String)> {
        None
    }
}

pub type DynFiles = Arc<dyn FileStore>;

// ───────────────────────────── Briefcase ─────────────────────────────

pub struct BriefcaseFiles {
    http: reqwest::Client,
    api_url: String,
    web_url: String,
    iam: DynIam,
}

impl BriefcaseFiles {
    pub fn new(api_url: String, web_url: String, iam: DynIam) -> Self {
        Self { http: reqwest::Client::new(), api_url: api_url.trim_end_matches('/').to_owned(), web_url: web_url.trim_end_matches('/').to_owned(), iam }
    }

    async fn delegated(
        &self,
        silicon: &Principal,
        endpoint_id: &str,
        path: &str,
        metadata: serde_json::Value,
        body: Vec<u8>,
        content_type: &str,
        sel: Option<&TestingSelection>,
    ) -> AppResult<serde_json::Value> {
        let proof = self.iam.obo_proof(silicon, "briefcase", endpoint_id, metadata, "POST", &body, sel).await?;
        let mut req = self
            .http
            .post(format!("{}{path}", self.api_url))
            .header("X-App-ID", self.iam.app_id())
            .header("X-IAM-OBO-Access-Proof", &proof.access_proof)
            .header("Content-Type", content_type)
            .body(body);
        if let Some(team) = &silicon.team {
            req = req.header("X-Org-ID", team);
        }
        if let Some(secret) = &proof.testing_app_secret {
            req = req.header("X-Briefcase-App-Secret", secret);
        }
        let resp = req.send().await.map_err(|e| AppError::unavailable("Briefcase", e))?;
        let status = resp.status();
        let json: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        if !status.is_success() {
            let msg = json.pointer("/error/message").or_else(|| json.pointer("/data/message")).and_then(|m| m.as_str()).unwrap_or("no details");
            return Err(AppError::unavailable("Briefcase", format!("{endpoint_id} answered {status}: {msg}")));
        }
        Ok(json)
    }
}

#[async_trait]
impl FileStore for BriefcaseFiles {
    async fn store(&self, silicon: &Principal, file: NewFile<'_>, sel: Option<&TestingSelection>) -> AppResult<Stored> {
        let entry = self
            .delegated(
                silicon,
                "briefcase.files.create",
                "/api/v1/obo/files",
                serde_json::json!({"path": "", "name": file.name, "content_type": file.content_type}),
                file.bytes,
                "application/octet-stream",
                sel,
            )
            .await?;
        let entry = entry.get("data").cloned().unwrap_or(entry);
        let file_id = entry
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| AppError::unavailable("Briefcase", "the created entry had no id"))?;
        let team = silicon.team.clone().unwrap_or_default();
        let url = entry.get("permanent_url").and_then(|v| v.as_str()).map(str::to_owned).unwrap_or_else(|| {
            format!("{}/org/{team}/apps/{}/private/{}/{}", self.web_url, self.iam.app_id(), silicon.id(), file.name)
        });
        // Share with the device's owner: create, read and update, never delete.
        let invite = serde_json::json!({
            "operation_id": Uuid::new_v4(),
            "entry_id": file_id,
            "invitation": {"principal": {"type": "carbon", "id": file.owner_carbon}, "access": ["read", "write", "update"], "inherit": true}
        });
        let body = serde_json::to_vec(&invite).unwrap_or_default();
        let shared_with = match self
            .delegated(silicon, "briefcase.invitations.create", "/api/v1/obo/invitations", serde_json::json!({}), body, "application/json", sel)
            .await
        {
            Ok(_) => Some(file.owner_carbon.to_owned()),
            Err(e) => {
                tracing::warn!(file_id = %file_id, error = %e.0.message, "sharing an Extend file with the device owner failed");
                None
            }
        };
        Ok(Stored { file_id, url, shared_with })
    }

    async fn destroy(&self, silicon: &Principal, file_id: Uuid, sel: Option<&TestingSelection>) -> AppResult<()> {
        let body = serde_json::to_vec(&serde_json::json!({"entry_id": file_id})).unwrap_or_default();
        self.delegated(silicon, "briefcase.entries.trash", "/api/v1/obo/entries/trash", serde_json::json!({}), body, "application/json", sel)
            .await
            .map(|_| ())
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
        Ok(Self { dir: dir.to_owned(), public_url: public_url.trim_end_matches('/').to_owned() })
    }
}

#[async_trait]
impl FileStore for LocalFiles {
    async fn store(&self, _silicon: &Principal, file: NewFile<'_>, _sel: Option<&TestingSelection>) -> AppResult<Stored> {
        let file_id = Uuid::now_v7();
        tokio::fs::write(self.dir.join(file_id.to_string()), &file.bytes).await.map_err(AppError::internal)?;
        tokio::fs::write(self.dir.join(format!("{file_id}.type")), file.content_type).await.map_err(AppError::internal)?;
        Ok(Stored { file_id, url: format!("{}/dev/files/{file_id}", self.public_url), shared_with: Some(file.owner_carbon.to_owned()) })
    }

    async fn destroy(&self, _silicon: &Principal, file_id: Uuid, _sel: Option<&TestingSelection>) -> AppResult<()> {
        let _ = tokio::fs::remove_file(self.dir.join(file_id.to_string())).await;
        let _ = tokio::fs::remove_file(self.dir.join(format!("{file_id}.type"))).await;
        Ok(())
    }

    async fn read_local(&self, file_id: Uuid) -> Option<(Vec<u8>, String)> {
        let bytes = tokio::fs::read(self.dir.join(file_id.to_string())).await.ok()?;
        let ct = tokio::fs::read_to_string(self.dir.join(format!("{file_id}.type"))).await.unwrap_or_else(|_| "application/octet-stream".into());
        Some((bytes, ct))
    }
}

pub fn not_found() -> AppError {
    AppError::new(ErrorCode::FileNotFound, "The file does not exist, already self-destructed, or is not visible to you.")
}
