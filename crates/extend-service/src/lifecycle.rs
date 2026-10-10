//! What Silicon Accounts' events do in Extend (`POST /webhooks/accounts`).
//!
//! | event | effect |
//! |---|---|
//! | `account.id_changed` | the cached public id changes; everything stays keyed on the uuid |
//! | `account.updated` | cached name, photo and custodian, when the event's version is newer |
//! | `silicon.custodian_changed` | the new custodian sees the Silicon; the grants the previous custodian gave it end (their sessions end as `access_removed`); other Carbons' grants stay, and their devices' logs say the custodian changed |
//! | `membership.signed_out` (`app_revoked`) | Extend revoked one sign-in (a logout): what that account runs ends as in `POST /api/v2/auth/logout` (only sessions started before the logout; a no-op when the logout already ended them), its other sign-ins stay valid |
//! | `membership.signed_out` (any other reason), `membership.access_removed` | tokens issued before the event are refused; a Silicon's sessions end, a Carbon's Silicons' sessions on their pairs end; held proofs end; pairs and grants stay |
//! | `account.deleted` | as above, and: a Carbon's pairs end (their devices are unpaired); a Silicon's grants are archived and its wake requests withdrawn; the account shows as "deleted account" in others' history |
//!
//! Every effect is idempotent, so a retried delivery is harmless.

use extend_protocol::model::{EndReason, MemberKind, WakeEndReason};
use silicon_accounts_client::{WebhookEvent, WebhookPayload};
use time::OffsetDateTime;

use crate::db::World;
use crate::domain::{self, GrantEnd, RevokeScope};
use crate::error::AppResult;
use crate::state::AppState;

/// The account an event is about.
fn account_of(event: &WebhookEvent) -> Option<&str> {
    match &event.payload {
        WebhookPayload::AccountIdChanged(d) => Some(&d.uuid),
        WebhookPayload::AccountUpdated(d) => Some(&d.uuid),
        WebhookPayload::AccountDeleted(d) | WebhookPayload::MembershipAccessRemoved(d) => Some(&d.uuid),
        WebhookPayload::MembershipSignedOut(d) => Some(&d.uuid),
        WebhookPayload::CustodianChanged(d) => Some(&d.uuid),
        _ => None,
    }
}

/// Applies one verified event exactly once (deduplicated on `event_id`, serialized per account).
pub async fn receive(state: &AppState, event: &WebhookEvent) -> AppResult<()> {
    let world = World::production();
    let account = account_of(event).map(str::to_owned);
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 7342020))")
        .bind(format!(
            "extend-accounts-event:{}",
            account.as_deref().unwrap_or(&event.event_id)
        ))
        .execute(&mut *tx)
        .await?;
    let seen: bool = sqlx::query_scalar(sql!(
        "SELECT EXISTS (SELECT 1 FROM {} WHERE event_id = $1)",
        world.t("accounts_events")
    ))
    .bind(&event.event_id)
    .fetch_one(&mut *tx)
    .await?;
    if seen {
        tracing::info!(event_id = %event.event_id, "Silicon Accounts event already applied; acknowledged");
        tx.commit().await?;
        return Ok(());
    }
    apply(state, &world, event).await?;
    sqlx::query(sql!(
        "INSERT INTO {} (event_id, event_type, account_uuid, occurred_at) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        world.t("accounts_events")
    ))
    .bind(&event.event_id)
    .bind(&event.event_type)
    .bind(&account)
    .bind(event.occurred_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn apply(state: &AppState, world: &World, event: &WebhookEvent) -> AppResult<()> {
    let at = event.occurred_at.unwrap_or_else(OffsetDateTime::now_utc);
    let directory = &state.accounts.directory;
    match &event.payload {
        WebhookPayload::Ping => tracing::info!(event_id = %event.event_id, "Silicon Accounts ping"),
        WebhookPayload::AccountIdChanged(d) => {
            let kind = d.kind.map(|k| match k {
                silicon_accounts_client::AccountKind::Carbon => MemberKind::Carbon,
                silicon_accounts_client::AccountKind::Silicon => MemberKind::Silicon,
            });
            directory.set_id(&d.uuid, kind, &d.new_id, at).await?;
            state.accounts.forget(&d.uuid).await;
            tracing::info!(uuid = %d.uuid, from = %d.old_id, to = %d.new_id, "account id changed");
        }
        WebhookPayload::AccountUpdated(d) => {
            if let Some(a) = &d.account {
                let applied = directory.apply_update(a).await?;
                tracing::info!(uuid = %d.uuid, changed = ?d.changed, applied, "account updated");
            }
            state.accounts.forget(&d.uuid).await;
        }
        WebhookPayload::CustodianChanged(d) => {
            let to = d.to.as_ref().map(|t| (t.uuid.as_str(), t.id.as_str()));
            let cached = directory.set_custodian(&d.uuid, to).await?;
            let previous = d.from.as_ref().map(|f| f.uuid.clone()).or(cached);
            state.accounts.forget(&d.uuid).await;
            if let Some(prev) = previous.filter(|p| Some(p.as_str()) != to.map(|t| t.0)) {
                custodian_changed(state, world, &d.uuid, &prev).await?;
            }
        }
        WebhookPayload::MembershipSignedOut(d) => {
            if d.reason.as_deref() == Some("app_revoked") {
                // Extend revoked one sign-in of the account: a logout (one machine's CLI, or the
                // website). `POST /api/v2/auth/logout` already ended what it runs; when the CLI or
                // website could only revoke at Silicon Accounts directly, it ends now. Sessions
                // started after the logout stay, and the account's other sign-ins stay valid.
                let kind = directory.get(&d.uuid).await?.map(|r| r.kind());
                let ended = logged_out(state, world, &d.uuid, kind, Some(at), &domain::system_member()).await?;
                tracing::info!(uuid = %d.uuid, sessions = ?ended, "one sign-in of the account at Extend ended; its other sign-ins stay");
            } else {
                tracing::info!(uuid = %d.uuid, reason = ?d.reason, "the account signed out of Extend everywhere");
                access_ended(state, world, &d.uuid, at).await?;
            }
        }
        WebhookPayload::MembershipAccessRemoved(d) => {
            tracing::info!(uuid = %d.uuid, "the account removed Extend's access");
            access_ended(state, world, &d.uuid, at).await?;
        }
        WebhookPayload::AccountDeleted(d) => {
            tracing::info!(uuid = %d.uuid, "the account was deleted");
            deleted(state, world, &d.uuid, at).await?;
        }
        _ => {
            tracing::info!(event_id = %event.event_id, event_type = %event.event_type, "Silicon Accounts event Extend doesn't act on; acknowledged")
        }
    }
    Ok(())
}

/// One sign-in of an account at Extend ended (a logout): the proofs Extend holds for it end, and
/// what it runs ends: a Silicon's sessions (`silicon_logged_out`), or the sessions of the Silicons
/// a Carbon gave access to through their own pairs (`access_removed`, never another Carbon's
/// side). With `started_before`, sessions started later are left alone. Tokens stay valid: the
/// account may be signed in elsewhere. Returns the sessions it ended.
pub async fn logged_out(
    state: &AppState,
    world: &World,
    uuid: &str,
    kind: Option<MemberKind>,
    started_before: Option<OffsetDateTime>,
    actor: &extend_protocol::model::Member,
) -> AppResult<Vec<String>> {
    state.accounts.forget(uuid).await;
    state.proofs.drop_account(uuid).await;
    match kind {
        Some(MemberKind::Silicon) => {
            domain::end_silicon_sessions(state, world, uuid, started_before, EndReason::SiliconLoggedOut, actor).await
        }
        Some(MemberKind::Carbon) => {
            domain::end_carbon_side(state, world, uuid, started_before, EndReason::AccessRemoved, actor).await
        }
        None => Ok(Vec::new()),
    }
}

/// The account's sign-ins at Extend ended: tokens issued before `at` are refused, its running work
/// ends, and the proofs Extend held for it end. Pairs and grants stay.
pub async fn access_ended(state: &AppState, world: &World, uuid: &str, at: OffsetDateTime) -> AppResult<()> {
    let directory = &state.accounts.directory;
    let kind = directory.get(uuid).await?.map(|r| r.kind());
    directory.revoke_before(uuid, at).await?;
    state.accounts.forget(uuid).await;
    state.proofs.drop_account(uuid).await;
    let system = domain::system_member();
    match kind {
        Some(MemberKind::Silicon) => {
            let ended =
                domain::end_silicon_sessions(state, world, uuid, None, EndReason::SiliconLoggedOut, &system).await?;
            crate::wake::withdraw(
                state,
                world,
                crate::wake::Withdraw::Silicon { silicon_id: uuid },
                WakeEndReason::AccessRemoved,
            )
            .await;
            stop_pending_requests(state, world, uuid, "The Silicon signed out of Extend before delivery.").await?;
            tracing::info!(uuid, sessions = ?ended, "ended the Silicon's sessions");
        }
        Some(MemberKind::Carbon) => {
            let ended = domain::end_carbon_side(state, world, uuid, None, EndReason::AccessRemoved, &system).await?;
            tracing::info!(uuid, sessions = ?ended, "ended the sessions on the Carbon's pairs");
        }
        None => tracing::info!(uuid, "Extend never saw this account; recorded the sign-out only"),
    }
    Ok(())
}

async fn stop_pending_requests(state: &AppState, world: &World, silicon: &str, why: &str) -> AppResult<()> {
    sqlx::query(sql!(
        "UPDATE {} SET delivery = 'failed', last_error = $2, ting_next_at = NULL WHERE from_id = $1 AND delivery = 'pending'",
        world.t("requests")
    ))
    .bind(silicon)
    .bind(why)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// `account.deleted`.
pub async fn deleted(state: &AppState, world: &World, uuid: &str, at: OffsetDateTime) -> AppResult<()> {
    let kind = state.accounts.directory.get(uuid).await?.map(|r| r.kind());
    access_ended(state, world, uuid, at).await?;
    let system = domain::system_member();
    match kind {
        Some(MemberKind::Carbon) => {
            let pairs: Vec<String> = sqlx::query_scalar(sql!(
                "SELECT device_id FROM {} WHERE owner_id = $1 AND removed_at IS NULL AND host_device_id IS NULL",
                world.t("devices")
            ))
            .bind(uuid)
            .fetch_all(&state.pool)
            .await?;
            for device_id in pairs {
                domain::unpair_with(
                    state,
                    world,
                    &device_id,
                    EndReason::DeviceRemoved,
                    &system,
                    serde_json::json!({"reason": "account_deleted"}),
                )
                .await?;
            }
        }
        Some(MemberKind::Silicon) => {
            domain::revoke_grants(
                state,
                world,
                RevokeScope::Silicon { silicon_id: uuid },
                GrantEnd::AccountDeleted,
                &system,
            )
            .await?;
        }
        None => {}
    }
    // The account's own details go: telemetry no longer names it, and its Ting enrolment record
    // (when and why it was enrolled) is dropped.
    sqlx::query(sql!(
        "UPDATE {} SET member_id = NULL WHERE member_id = $1",
        world.t("telemetry")
    ))
    .bind(uuid)
    .execute(&state.pool)
    .await?;
    sqlx::query(sql!(
        "DELETE FROM {} WHERE account_uuid = $1",
        world.t("ting_enrolments")
    ))
    .bind(uuid)
    .execute(&state.pool)
    .await?;
    state.accounts.directory.mark_deleted(uuid, at).await?;
    Ok(())
}

/// A Silicon's custodian changed from `previous`: the grants `previous` gave it end; other Carbons'
/// grants stay, and their devices' logs say the custodian changed.
async fn custodian_changed(state: &AppState, world: &World, silicon: &str, previous: &str) -> AppResult<()> {
    let system = domain::system_member();
    let ended = domain::revoke_grants(
        state,
        world,
        RevokeScope::Granter {
            carbon_id: previous,
            silicon_id: silicon,
        },
        GrantEnd::CustodianChanged,
        &system,
    )
    .await?;
    let kept: Vec<String> = sqlx::query_scalar(sql!(
        "SELECT device_id FROM {} WHERE silicon_id = $1",
        world.t("device_access")
    ))
    .bind(silicon)
    .fetch_all(&state.pool)
    .await?;
    let shown = state.accounts.directory.public_id(silicon).await;
    for device_id in &kept {
        domain::log(
            state,
            world,
            device_id,
            &system,
            "custodian_changed",
            None,
            serde_json::json!({"silicon_id": shown, "silicon_uuid": silicon}),
        )
        .await;
    }
    tracing::info!(
        silicon,
        grants_ended = ended.len(),
        grants_kept = kept.len(),
        "a Silicon's custodian changed"
    );
    Ok(())
}
