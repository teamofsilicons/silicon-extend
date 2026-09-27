//! Files made in sessions: listing, Briefcase links, and keeping a file before it self-destructs.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
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
    team: String,
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
            team: Some(self.team),
        }
    }
}

const COLS: &str = "f.file_id, f.team, f.device_id, f.session_id, f.command_id, f.created_by, f.shared_with, f.name, f.kind, f.content_type, f.size_bytes, f.url, f.self_destruct_at, f.permanent, f.created_at";

#[derive(Deserialize)]
pub struct ListQuery {
    session_id: Option<String>,
    device_id: Option<String>,
    kind: Option<String>,
    limit: Option<i64>,
    cursor: Option<String>,
}

/// A Silicon sees the files it made in the Team it acts in; a Carbon, the files made on their pairs
/// in every Team (`$1`, the Team, is still mentioned so PostgreSQL can type it).
fn who(auth: &Auth) -> String {
    if auth.p.is_silicon() {
        "f.team = $1 AND f.created_by = $2".into()
    } else {
        format!(
            "($1::text IS NULL OR $1 IS NOT NULL) AND EXISTS (SELECT 1 FROM {} d WHERE d.device_id = f.device_id AND d.owner_id = $2)",
            auth.world.t("devices")
        )
    }
}

fn team_of(auth: &Auth) -> AppResult<Option<String>> {
    if auth.p.is_silicon() {
        Ok(Some(auth.team()?.to_owned()))
    } else {
        Ok(auth.p.team.clone())
    }
}

pub async fn list(State(state): State<Shared>, auth: Auth, Query(q): Query<ListQuery>) -> AppResult<Response> {
    let team = team_of(&auth)?;
    let lim = limit(q.limit)?;
    let before: Option<Uuid> = q
        .cursor
        .as_deref()
        .map(decode_cursor)
        .transpose()?
        .and_then(|c| c.parse().ok());
    let rows: Vec<FileRow> = sqlx::query_as(sql!(
        "SELECT {COLS} FROM {} f WHERE {}
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
    let team = team_of(auth)?;
    sqlx::query_as(sql!(
        "SELECT {COLS} FROM {} f WHERE f.file_id = $3 AND {} AND (f.self_destruct_at IS NULL OR f.self_destruct_at > now())",
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

/// Which bytes of a file a `Range` header asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteRange {
    /// No usable range (none, several, or malformed): the whole file.
    Full,
    /// First and last byte, inclusive.
    Part(u64, u64),
    /// A range that starts past the end of the file.
    Unsatisfiable,
}

/// Reads a single `bytes=` range (RFC 9110 section 14.2). Several ranges, other units and malformed
/// ranges are ignored (the whole file is served), which the RFC allows.
pub fn byte_range(value: &str, total: u64) -> ByteRange {
    let Some(spec) = value.trim().strip_prefix("bytes=") else {
        return ByteRange::Full;
    };
    if spec.contains(',') {
        return ByteRange::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return ByteRange::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        // The last N bytes.
        return match last.parse::<u64>() {
            Ok(0) => ByteRange::Unsatisfiable,
            Ok(_) if total == 0 => ByteRange::Unsatisfiable,
            Ok(n) => ByteRange::Part(total - n.min(total), total - 1),
            Err(_) => ByteRange::Full,
        };
    }
    let Ok(start) = first.parse::<u64>() else {
        return ByteRange::Full;
    };
    let end = if last.is_empty() {
        None
    } else {
        match last.parse::<u64>() {
            Ok(e) if e >= start => Some(e),
            _ => return ByteRange::Full,
        }
    };
    if start >= total {
        return ByteRange::Unsatisfiable;
    }
    ByteRange::Part(start, end.unwrap_or(u64::MAX).min(total - 1))
}

/// `attachment` with the file's name, as plain ASCII and as RFC 8187 UTF-8.
pub fn content_disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if (c.is_ascii_graphic() && c != '"' && c != '\\') || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

/// A file's bytes (`GET /api/v1/files/{file_id}/content`), for the Silicon that made it and the
/// Carbon who owns its device. They are read from Briefcase as the caller, so Briefcase's own
/// sharing applies too. One `Range: bytes=` range is honoured (206, or 416 past the end).
pub async fn content(
    State(state): State<Shared>,
    auth: Auth,
    Path(file_id): Path<Uuid>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let f = visible(&state, &auth, file_id).await?;
    // Read through Briefcase as the caller, in the file's Team: a Carbon's login must reach it.
    let mut reader = auth.p.clone();
    if auth.p.is_carbon() && auth.p.team.as_deref() != Some(f.team.as_str()) {
        let sign_in = || {
            AppError::new(
                ErrorCode::NotATeamMember,
                format!("{}'s Extend login doesn't reach {}.", auth.p.id(), f.team),
            )
            .hint(format!("Sign in to Extend for {} to open files made there.", f.team))
        };
        if !auth.p.teams.contains(&f.team) {
            return Err(sign_in());
        }
        reader = state
            .authorize(&auth.p.token, Some(&f.team), auth.sel.as_ref())
            .await
            .map_err(|_| sign_in())?;
        reader.team = Some(f.team.clone());
    }
    let (bytes, stored_type) = match state.files.read(&reader, file_id, auth.sel.as_ref()).await {
        Ok(found) => found,
        Err(e)
            if auth.p.is_carbon()
                && f.shared_with.is_none()
                && matches!(e.code(), ErrorCode::NoAccess | ErrorCode::FileNotFound) =>
        {
            return Err(e.hint(format!(
                "Sharing {} with you failed when it was made, so Briefcase won't let you read it. Ask {} to share it with you in Briefcase.",
                f.name, f.created_by
            )));
        }
        Err(e) => return Err(e),
    };
    let content_type = if f.content_type.trim().is_empty() {
        stored_type
    } else {
        f.content_type.clone()
    };
    let total = bytes.len() as u64;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map_or(ByteRange::Full, |v| byte_range(v, total));
    let (status, body, content_range) = match range {
        ByteRange::Full => (StatusCode::OK, bytes, None),
        ByteRange::Part(first, last) => (
            StatusCode::PARTIAL_CONTENT,
            bytes[first as usize..=last as usize].to_vec(),
            Some(format!("bytes {first}-{last}/{total}")),
        ),
        ByteRange::Unsatisfiable => {
            let mut resp = AppError::invalid(format!(
                "The requested range starts past the end of {}, which is {total} bytes.",
                f.name
            ))
            .hint("Ask for a range inside the file, or leave out the Range header to get all of it.")
            .into_response();
            *resp.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{total}")) {
                resp.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return Ok(resp);
        }
    };
    let length = body.len();
    let mut resp = (status, body).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if let Ok(v) = HeaderValue::from_str(&content_disposition(&f.name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Some(r) = content_range.and_then(|r| HeaderValue::from_str(&r).ok()) {
        h.insert(header::CONTENT_RANGE, r);
    }
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    // Files are whatever a device made: never let a browser run one in Extend's origin.
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    Ok(resp)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_one_byte_range() {
        use ByteRange::*;
        assert_eq!(byte_range("bytes=0-3", 10), Part(0, 3));
        assert_eq!(byte_range("bytes=4-", 10), Part(4, 9));
        assert_eq!(byte_range("bytes=-3", 10), Part(7, 9));
        assert_eq!(byte_range("bytes=-30", 10), Part(0, 9));
        assert_eq!(byte_range("bytes=2-99", 10), Part(2, 9));
        assert_eq!(byte_range("bytes=10-", 10), Unsatisfiable);
        assert_eq!(byte_range("bytes=-0", 10), Unsatisfiable);
        assert_eq!(byte_range("bytes=0-", 0), Unsatisfiable);
        // Ignored: several ranges, other units, nonsense, last before first.
        assert_eq!(byte_range("bytes=0-1,4-5", 10), Full);
        assert_eq!(byte_range("items=0-1", 10), Full);
        assert_eq!(byte_range("bytes=x-1", 10), Full);
        assert_eq!(byte_range("bytes=5-2", 10), Full);
    }

    #[test]
    fn names_the_file_for_download() {
        assert_eq!(
            content_disposition("shot.png"),
            "attachment; filename=\"shot.png\"; filename*=UTF-8''shot.png"
        );
        assert_eq!(
            content_disposition("a \"b\" é.txt"),
            "attachment; filename=\"a _b_ _.txt\"; filename*=UTF-8''a%20%22b%22%20%C3%A9.txt"
        );
    }
}
