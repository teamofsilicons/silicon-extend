//! The local Silicon Accounts stand-in signs the CLI in the way Silicon Accounts does: a
//! short-lived token from `POST /dev/accounts/slt`, exchanged by the official client as a public
//! client at `/dev/accounts/v1/oauth/token`, refresh tokens that rotate (and end the sign-in when
//! one is reused), and revocation that Extend sees at once. `e2e/cli-e2e.sh` signs the real CLI in
//! this way.

mod common;

use common::*;
use extend_service::accounts::api::AccountsApi as _;
use serde_json::{Value, json};
use silicon_extend_client::auth::SignIn;

async fn mint_slt(env: &Env, id: &str, custodian: Option<&str>) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{}/dev/accounts/slt", env.base))
        .json(&json!({"type": "slt", "data": {"id": id, "custodian": custodian}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["data"]["expires_in"], 120);
    assert_eq!(v["data"]["account"]["uuid"], uuid(id).as_str());
    v["data"]["slt"].as_str().unwrap().to_owned()
}

fn sign_in(env: &Env) -> SignIn {
    SignIn::new(&format!("{}/dev/accounts", env.base)).unwrap()
}

#[tokio::test]
async fn a_short_lived_token_signs_the_cli_in_once() {
    let env = start().await;
    let slt = mint_slt(&env, "si:chef", Some("c:alice")).await;
    let tokens = sign_in(&env).exchange_slt(&slt).await.unwrap();
    let account = tokens.account.clone().unwrap();
    assert_eq!(
        (account.uuid.as_str(), account.id.as_str()),
        (uuid("si:chef").as_str(), "si:chef")
    );
    assert_eq!(account.custodian.as_ref().map(|c| c.id.as_str()), Some("c:alice"));
    assert!(
        tokens
            .refresh_token
            .as_ref()
            .is_some_and(|r| r.expose().starts_with("sar_local_"))
    );
    let (s, me) = api(&env, "GET", "/api/v2/me", tokens.access_token.expose(), None).await;
    assert_eq!((s, me["data"]["id"].as_str()), (200, Some("si:chef")), "{me}");

    let again = sign_in(&env).exchange_slt(&slt).await.unwrap_err();
    assert_eq!(again.code, "slt_already_used", "{again:?}");
    let unknown = sign_in(&env)
        .exchange_slt("slt_local_not-a-token-this-stand-in-made")
        .await
        .unwrap_err();
    assert_eq!(unknown.code, "slt_unknown", "{unknown:?}");
    let alice = env.accounts().ensure("c:alice", None).unwrap();
    let stale = env.accounts().mint_slt_with_ttl(&alice, -5);
    let expired = sign_in(&env).exchange_slt(&stale).await.unwrap_err();
    assert_eq!(expired.code, "slt_expired", "{expired:?}");
    // Another app's client id is refused before the token is spent.
    let fresh = mint_slt(&env, "c:alice", None).await;
    let other = SignIn::for_app(&format!("{}/dev/accounts", env.base), "remind", None).unwrap();
    assert!(other.exchange_slt(&fresh).await.is_err());
    assert!(sign_in(&env).exchange_slt(&fresh).await.is_ok());
}

#[tokio::test]
async fn refresh_tokens_rotate_and_a_reused_one_ends_the_sign_in() {
    let env = start().await;
    let first = sign_in(&env)
        .exchange_slt(&mint_slt(&env, "c:alice", None).await)
        .await
        .unwrap();
    let old = first.refresh_token.clone().unwrap();
    let second = sign_in(&env).refresh(old.expose()).await.unwrap();
    let new = second.refresh_token.clone().unwrap();
    assert_ne!(old.expose(), new.expose());
    let (s, _) = api(&env, "GET", "/api/v2/me", second.access_token.expose(), None).await;
    assert_eq!(s, 200);
    // Presenting the used one again ends the sign-in, so the newer one stops working too.
    let reused = sign_in(&env).refresh(old.expose()).await.unwrap_err();
    assert!(reused.sign_in_ended(), "{reused:?}");
    let after = sign_in(&env).refresh(new.expose()).await.unwrap_err();
    assert!(after.sign_in_ended(), "{after:?}");
    let seen = env.accounts().introspect(second.access_token.expose()).await.unwrap();
    assert!(
        !seen.active,
        "a reused refresh token must end the sign-in's access tokens too"
    );
}

#[tokio::test]
async fn revoking_a_sign_in_ends_it_and_leaves_the_others() {
    let env = start().await;
    let one = sign_in(&env)
        .exchange_slt(&mint_slt(&env, "si:chef", Some("c:alice")).await)
        .await
        .unwrap();
    let two = sign_in(&env)
        .exchange_slt(&mint_slt(&env, "si:chef", Some("c:alice")).await)
        .await
        .unwrap();
    let revoked = sign_in(&env)
        .revoke(one.refresh_token.as_ref().unwrap().expose())
        .await
        .unwrap();
    assert!(revoked.revoked);
    assert!(
        !env.accounts()
            .introspect(one.access_token.expose())
            .await
            .unwrap()
            .active
    );
    assert!(
        env.accounts()
            .introspect(two.access_token.expose())
            .await
            .unwrap()
            .active
    );
    // Extend's own logout revokes the refresh token it is given, through the same stand-in.
    let (s, v) = api(
        &env,
        "POST",
        "/api/v2/auth/logout",
        two.access_token.expose(),
        Some(json!({"type": "logout", "data": {"refresh_token": two.refresh_token.as_ref().unwrap().expose()}})),
    )
    .await;
    assert!(s < 300, "{s} {v}");
    assert!(
        !env.accounts()
            .introspect(two.access_token.expose())
            .await
            .unwrap()
            .active
    );
    let unknown = sign_in(&env).revoke("sar_local_unknown-token").await.unwrap();
    assert!(!unknown.revoked);
}
