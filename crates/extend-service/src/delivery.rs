//! Sending Tings, and what Extend records about Ting: which of its types Ting doesn't know
//! (`ting_types`), and whether each account's enrolment reaches them (`ting_enrolments`).
//!
//! Extend sends every Ting as itself (an App verification proof, see crate::ting). When Ting is
//! off (`EXTEND_TING_URL` unset), an attempt sends nothing and says so ([`crate::ting::OFF`]); the
//! request or wake request stays visible on the website, in the CLI and on the device.

use serde_json::Value;
use time::OffsetDateTime;

use crate::accounts::Principal;
use crate::db::World;
use crate::state::AppState;
use crate::ting;

/// What one attempt at a Ting did.
#[derive(Debug, Clone, Default)]
pub struct Attempt {
    pub delivered: bool,
    /// Whether Extend tried (an attempt while Ting is off doesn't count).
    pub tried: bool,
    /// What went wrong, plainly (message and hint).
    pub error: Option<String>,
    /// Ting answered `recipient_not_registered`: stop retrying until the recipient enrols.
    pub not_registered: bool,
    /// Ting doesn't know the type: retried every 10 minutes, and shown.
    pub missing_type: bool,
    /// Ting is off on this server: nothing was sent, and nothing is retried.
    pub disabled: bool,
}

/// Seconds before the next try after `attempts` counted attempts: 30 s, then 1, 2, 4 and 8 minutes,
/// then every 8 minutes.
pub fn backoff(attempts: i32) -> time::Duration {
    let secs = match attempts {
        i32::MIN..=1 => 30,
        2 => 60,
        3 => 120,
        4 => 240,
        _ => 480,
    };
    time::Duration::seconds(secs)
}

/// How long a Ting whose type Ting doesn't know waits before it is tried again.
pub const MISSING_TYPE_RETRY: time::Duration = time::Duration::minutes(10);

fn explain(e: &crate::error::AppError) -> String {
    match &e.0.hint {
        Some(h) => format!("{} {h}", e.0.message),
        None => e.0.message.clone(),
    }
}

/// Sends a frozen body, and records what Ting said about the type and the recipient.
pub async fn send(state: &AppState, world: &World, body: &Value) -> Attempt {
    if !state.notifier.enabled() {
        return Attempt {
            disabled: true,
            error: Some(ting::OFF.to_owned()),
            ..Attempt::default()
        };
    }
    let (ty, recipient) = ting::body_parts(body);
    let mut attempt = Attempt {
        tried: true,
        ..Attempt::default()
    };
    match state.notifier.send_frozen(body).await {
        Ok(()) => {
            attempt.delivered = true;
            type_known(state, world, &ty).await;
            reached(state, world, &recipient).await;
        }
        Err(e) => {
            attempt.error = Some(explain(&e));
            if let Some(missing) = ting::missing_type(&e) {
                attempt.missing_type = true;
                type_missing(state, world, &missing, &explain(&e)).await;
            } else if ting::not_registered(&e) {
                attempt.not_registered = true;
                refused(state, world, &recipient).await;
            }
        }
    }
    attempt
}

async fn type_known(state: &AppState, world: &World, ty: &str) {
    let _ = sqlx::query(sql!("DELETE FROM {} WHERE ting_type = $1", world.t("ting_types")))
        .bind(ty)
        .execute(&state.pool)
        .await;
}

async fn type_missing(state: &AppState, world: &World, ty: &str, error: &str) {
    let res = sqlx::query(sql!(
        "INSERT INTO {} (ting_type, missing_since, last_checked_at, last_error) VALUES ($1, now(), now(), $2)
         ON CONFLICT (ting_type) DO UPDATE SET last_checked_at = now(), last_error = EXCLUDED.last_error",
        world.t("ting_types")
    ))
    .bind(ty)
    .bind(error)
    .execute(&state.pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, ty, "recording a missing Ting type failed");
    }
    tracing::warn!(
        ty,
        "Ting doesn't know one of Extend's notification types; it must be registered in Ting"
    );
}

/// A Ting reached `account`: they receive Extend's notifications.
async fn reached(state: &AppState, world: &World, account: &str) {
    let _ = sqlx::query(sql!(
        "UPDATE {} SET refused_at = NULL, last_error = NULL WHERE account_uuid = $1 AND refused_at IS NOT NULL",
        world.t("ting_enrolments")
    ))
    .bind(account)
    .execute(&state.pool)
    .await;
}

/// Ting refused a Ting to `account` as not enrolled. It counts as turning Extend off only when
/// Extend had enrolled them; an account never enrolled stays pending, with why.
async fn refused(state: &AppState, world: &World, account: &str) {
    let _ = sqlx::query(sql!(
        "INSERT INTO {t} (account_uuid, last_error) VALUES ($1, $2)
         ON CONFLICT (account_uuid) DO UPDATE SET
             refused_at = CASE WHEN {t}.registered_at IS NOT NULL THEN now() ELSE {t}.refused_at END,
             last_error = CASE WHEN {t}.registered_at IS NOT NULL
                               THEN 'Turned off in Ting: Extend''s notifications are refused.'
                               ELSE EXCLUDED.last_error END",
        t = world.t("ting_enrolments")
    ))
    .bind(account)
    .bind("Not enrolled yet: Extend enrols an account at its next use of Extend.")
    .execute(&state.pool)
    .await;
}

/// Enrols `member` with Ting (their sign-in is live) and records the result. `force` is "Turn
/// on": it enrols again even when Extend has a record.
pub async fn register(state: &AppState, world: &World, member: &Principal, force: bool) -> crate::error::AppResult<()> {
    if !state.notifier.enabled() {
        return Ok(());
    }
    match state.notifier.register_recipient(member, force).await {
        Ok(()) => {
            sqlx::query(sql!(
                "INSERT INTO {} (account_uuid, registered_at) VALUES ($1, now())
                 ON CONFLICT (account_uuid) DO UPDATE SET registered_at = now(), refused_at = NULL, last_error = NULL",
                world.t("ting_enrolments")
            ))
            .bind(member.uuid())
            .execute(&state.pool)
            .await?;
            // Tings that stopped because this account wasn't enrolled go again now.
            retry_now_for(state, world, member.uuid()).await;
            Ok(())
        }
        Err(e) => {
            // Only an answer from Ting is recorded: an outage leaves no row, so the next chance to
            // enrol (a grant, a claim, a session) tries again.
            if e.code() != extend_protocol::ErrorCode::ServiceUnavailable {
                let _ = sqlx::query(sql!(
                    "INSERT INTO {} (account_uuid, last_error) VALUES ($1, $2)
                     ON CONFLICT (account_uuid) DO UPDATE SET last_error = EXCLUDED.last_error",
                    world.t("ting_enrolments")
                ))
                .bind(member.uuid())
                .bind(explain(&e))
                .execute(&state.pool)
                .await;
            }
            Err(e)
        }
    }
}

/// Enrols a Carbon only when Extend has no record for them, so a Carbon who turned Extend off in
/// Ting stays off. Runs in the background; failures are logged.
pub fn register_carbon_if_new(state: &crate::state::Shared, world: &World, carbon: &Principal) {
    if !state.notifier.enabled() {
        return;
    }
    let (state, world, carbon) = (state.clone(), world.clone(), carbon.clone());
    tokio::spawn(async move {
        let known: Option<(i32,)> = sqlx::query_as(sql!(
            "SELECT 1 FROM {} WHERE account_uuid = $1",
            world.t("ting_enrolments")
        ))
        .bind(carbon.uuid())
        .fetch_optional(&state.pool)
        .await
        .unwrap_or(None);
        if known.is_some() {
            return;
        }
        if let Err(e) = register(&state, &world, &carbon, false).await {
            tracing::warn!(carbon = carbon.public_id(), error = %e.0.message, "enrolling the Carbon with Ting failed");
        }
    });
}

/// Enrols a Silicon in the background, as at a session start.
pub fn register_silicon(state: &crate::state::Shared, world: &World, silicon: &Principal) {
    if !state.notifier.enabled() {
        return;
    }
    let (state, world, silicon) = (state.clone(), world.clone(), silicon.clone());
    tokio::spawn(async move {
        if let Err(e) = register(&state, &world, &silicon, false).await {
            tracing::warn!(
                silicon = silicon.public_id(),
                error = %e.0.message,
                hint = ?e.0.hint,
                "enrolling the Silicon to receive Tings failed; Tings to it stay pending and are retried"
            );
        }
    });
}

/// Makes every Ting to `account` that is waiting due now (after an enrolment).
pub async fn retry_now_for(state: &AppState, world: &World, account: &str) {
    let _ = sqlx::query(sql!(
        "UPDATE {} SET ting_next_at = now()
         WHERE delivery = 'pending'
           AND ((routed_to = 'holder' AND to_id = $1) OR (routed_to = 'carbon' AND routed_to_id = $1))",
        world.t("requests")
    ))
    .bind(account)
    .execute(&state.pool)
    .await;
    let _ = sqlx::query(sql!(
        "UPDATE {} SET ting_next_at = now() WHERE ting_delivery = 'pending' AND to_id = $1",
        world.t("wake_requests")
    ))
    .bind(account)
    .execute(&state.pool)
    .await;
    let _ = sqlx::query(sql!(
        "UPDATE {} SET answer_ting_next_at = now() WHERE answer_ting = 'pending' AND from_id = $1",
        world.t("wake_requests")
    ))
    .bind(account)
    .execute(&state.pool)
    .await;
}

/// Extend's types Ting answered unknown, by full name.
pub async fn missing_types(state: &AppState, world: &World) -> Vec<String> {
    sqlx::query_scalar(sql!(
        "SELECT ting_type FROM {} ORDER BY ting_type",
        world.t("ting_types")
    ))
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
}

/// An account's Ting enrolment, as `GET /ting-registration` shows it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RecipientRow {
    pub registered_at: Option<OffsetDateTime>,
    pub refused_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
}

impl RecipientRow {
    /// pending: never enrolled (or refused while never enrolled); off: refused after the last
    /// enrolment; on: otherwise.
    pub fn status(row: Option<&RecipientRow>) -> extend_protocol::model::TingStatus {
        use extend_protocol::model::TingStatus;
        match row {
            None => TingStatus::Pending,
            Some(r) => match (r.registered_at, r.refused_at) {
                (None, _) => TingStatus::Pending,
                (Some(reg), Some(refused)) if refused > reg => TingStatus::Off,
                _ => TingStatus::On,
            },
        }
    }
}

pub async fn recipient(state: &AppState, world: &World, account: &str) -> Option<RecipientRow> {
    sqlx::query_as(sql!(
        "SELECT registered_at, refused_at, last_error FROM {} WHERE account_uuid = $1",
        world.t("ting_enrolments")
    ))
    .bind(account)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_stays_at_eight_minutes() {
        let secs: Vec<i64> = (1..=7).map(|n| backoff(n).whole_seconds()).collect();
        assert_eq!(secs, vec![30, 60, 120, 240, 480, 480, 480]);
    }

    #[test]
    fn registration_status() {
        use extend_protocol::model::TingStatus;
        let t = OffsetDateTime::now_utc();
        let row = |reg: Option<OffsetDateTime>, refused: Option<OffsetDateTime>| RecipientRow {
            registered_at: reg,
            refused_at: refused,
            last_error: None,
        };
        assert_eq!(RecipientRow::status(None), TingStatus::Pending);
        assert_eq!(RecipientRow::status(Some(&row(None, Some(t)))), TingStatus::Pending);
        assert_eq!(RecipientRow::status(Some(&row(Some(t), None))), TingStatus::On);
        assert_eq!(
            RecipientRow::status(Some(&row(Some(t), Some(t + time::Duration::seconds(1))))),
            TingStatus::Off
        );
    }
}
