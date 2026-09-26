//! Files made in sessions: listing, Briefcase links, and keeping a file before it self-destructs.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use extend_protocol::ErrorCode;
use extend_protocol::model::{FileInfo, FileKind};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{decode_cursor, encode_cursor, limit, ok};
use crate::error::{AppError, AppResult};
use crate::files::not_found;
use crate::state::{Auth, Shared};

#[derive(sqlx::FromRow)]
struct FileRow {
    file_id: Uuid,
    device_id: String,
    session_id: Option<String>,
    command_id: Option<Uuid>,
    created_by: String,
    shared_with: Option<String>,
    name: String,
    kind: String,
    content_type: String,
    size_bytes: i64,
    url: String,
    self_destruct_at: Option<OffsetDateTime>,
    permanent: bool,
    created_at: OffsetDateTime,
}

impl FileRow {
    fn view(self) -> FileInfo {
        FileInfo {
            file_id: self.file_id,
            name: self.name,
            kind: FileKind::parse(&self.kind),
            content_type: self.content_type,
            size_bytes: self.size_bytes,
            url: self.url,
            self_destruct_at: self.self_destruct_at,
            permanent: self.permanent,
            session_id: self.session_id.and_then(|s| s.parse().ok()),
            device_id: self.device_id.parse().ok(),
            command_id: self.command_id,
            created_by: Some(self.created_by),
            shared_with: self.shared_with,
            created_at: Some(self.created_at),
        }
    }
}

const COLS: &str = "f.file_id, f.device_id, f.session_id, f.command_id, f.created_by, f.shared_with, f.name, f.kind, f.content_type, f.size_bytes, f.url, f.self_destruct_at, f.permanent, f.created_at";

#[derive(Deserialize)]
pub struct ListQuery {
    session_id: Option<String>,
    device_id: Option<String>,
    kind: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
}

fn who(auth: &Auth) -> String {
    if auth.p.is_silicon() {
        "f.created_by = $2".into()
    } else {
        format!(
            "EXISTS (SELECT 1 FROM {} d WHERE d.device_id = f.device_id AND d.owner_id = $2)",
            auth.world.t("devices")
        )
    }
}

pub async fn list(State(state): State<Shared>, auth: Auth, Query(q): Query<ListQuery>) -> AppResult<Response> {
    let team = auth.team()?.to_owned();
    let lim = limit(q.limit)?;
    let before: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(decode_cursor)
        .transpose()?
        .and_then(|c| c.parse().ok());
    let rows: Vec<FileRow> = sqlx::query_as(sql!(
        "SELECT {COLS} FROM {} f WHERE f.team = $1 AND {}
           AND (f.self_destruct_at IS NULL OR f.self_destruct_at > now())
           AND ($3::text IS NULL OR f.session_id = $3) AND ($4::text IS NULL OR f.device_id = $4) AND ($5::text IS NULL OR f.kind = $5)
           AND ($6::uuid IS NULL OR f.file_id < $6)
         ORDER BY f.file_id DESC LIMIT $7",
        auth.world.t("files"),
        who(&auth)
    ))
    .bind(&team)
    .bind(auth.p.id())
    .bind(&q.session_id)
    .bind(&q.device_id)
    .bind(&q.kind)
    .bind(before)
    .bind(lim + 1)
    .fetch_all(&state.pool)
    .await?;
    let more = rows.len() as i64 > lim;
    let items: Vec<FileInfo> = rows.into_iter().take(lim as usize).map(FileRow::view).collect();
    let next = more.then(|| encode_cursor(&items.last().map(|f| f.file_id.to_string()).unwrap_or_default()));
    Ok(ok("files", serde_json::json!({"items": items, "next_cursor": next})))
}

async fn visible(state: &Shared, auth: &Auth, file_id: Uuid) -> AppResult<FileRow> {
    let team = auth.team()?.to_owned();
    sqlx::query_as(sql!(
        "SELECT {COLS} FROM {} f WHERE f.file_id = $3 AND f.team = $1 AND {} AND (f.self_destruct_at IS NULL OR f.self_destruct_at > now())",
        auth.world.t("files"),
        who(auth)
    ))
    .bind(&team)
    .bind(auth.p.id())
    .bind(file_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(not_found)
}

pub async fn get(State(state): State<Shared>, auth: Auth, Path(file_id): Path<Uuid>) -> AppResult<Response> {
    Ok(ok("file", visible(&state, &auth, file_id).await?.view()))
}

pub async fn keep(State(state): State<Shared>, auth: Auth, Path(file_id): Path<Uuid>) -> AppResult<Response> {
    let f = visible(&state, &auth, file_id).await?;
    if f.created_by != auth.p.id() {
        return Err(AppError::new(
            ErrorCode::NotSessionOwner,
            format!(
                "Only {} (the Silicon that made it) can make this file permanent.",
                f.created_by
            ),
        ));
    }
    if f.permanent {
        return Err(AppError::new(
            ErrorCode::NotSelfDestructing,
            "This file is already permanent.",
        ));
    }
    sqlx::query(sql!(
        "UPDATE {} SET permanent = true, self_destruct_at = NULL WHERE file_id = $1",
        auth.world.t("files")
    ))
    .bind(file_id)
    .execute(&state.pool)
    .await?;
    Ok(ok("file", visible(&state, &auth, file_id).await?.view()))
}

/// Serves a file kept on local disk (development and tests only).
pub async fn local_file(State(state): State<Shared>, Path(file_id): Path<Uuid>) -> Response {
    match state.files.read_local(file_id).await {
        Some((bytes, ct)) => {
            let mut resp = (StatusCode::OK, bytes).into_response();
            if let Ok(v) = HeaderValue::from_str(&ct) {
                resp.headers_mut().insert("content-type", v);
            }
            resp
        }
        None => not_found().into_response(),
    }
}
