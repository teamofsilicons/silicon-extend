//! `GET /api/v2/accounts/lookup?id=si:scout`: who an id belongs to (for the website's access
//! picker and the CLI's confirmations), through Silicon Accounts.

use axum::extract::{Query, State};
use axum::response::Response;
use extend_protocol::account::AccountRef;
use serde::Deserialize;

use super::ok;
use crate::error::{AppError, AppResult};
use crate::state::{Auth, Shared};

#[derive(Deserialize)]
pub struct LookupQuery {
    id: Option<String>,
}

pub async fn lookup(State(state): State<Shared>, _auth: Auth, Query(q): Query<LookupQuery>) -> AppResult<Response> {
    let id =
        q.id.as_deref()
            .map(str::trim)
            .filter(|i| !i.is_empty())
            .ok_or_else(|| AppError::invalid("Send the id to look up: ?id=si:scout (or c:ada)."))?;
    let row = state.accounts.directory.resolve(id, None).await?;
    Ok(ok(
        "account",
        AccountRef::new(row.uuid.clone(), row.shown_id(), row.kind())
            .display_name(row.display_name.clone())
            .pfp_url(row.pfp_url.clone()),
    ))
}
