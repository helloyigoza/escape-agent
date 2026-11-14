//! Unit tests for [`AntigravityService`](super::AntigravityService).

use super::{AntigravityService, now_ms};
use crate::config::AntigravityConfig;
use crate::constants::{ANTIGRAVITY_METHOD_ID, ANTIGRAVITY_PROVIDER_ID, AntigravityEndpoints};
use crate::oauth::AntigravityTokenExchange;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router as AxumRouter};
use oc_store::{NewCredential, Store};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::net::TcpListener;

async fn serve(app: AxumRouter) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://127.0.0.1:{port}")
}

fn oauth_credential(email: &str) -> Value {
    json!({
        "type": "oauth",
        "methodID": ANTIGRAVITY_METHOD_ID,
        "refresh": "1//stored-token|proj-1",
        "access": "ya29.stale",
        "expires": 1,
        "metadata": {
            "email": email,
            "projectId": "proj-1",
            "managedProjectId": "managed-9"
        }
    })
}

#[tokio::test]
async fn token_refresh_preserves_credential_metadata_and_persists_the_row() {
    let base = serve(Router::new().route(
        "/token",
        post(|| async {
            (
                StatusCode::OK,
                Json(json!({ "access_token": "ya29.fresh", "expires_in": 3600 })),
            )
        }),
    ))
    .await;

    let store = Arc::new(Store::open_memory().await.expect("store"));
    let created = store
        .create_credential(NewCredential {
            id: None,
            integration_id: ANTIGRAVITY_PROVIDER_ID.to_owned(),
            label: Some("work".to_owned()),
            value: oauth_credential("user@example.com"),
            active: Some(true),
        })
        .await
        .expect("create credential");
    let cred_id = created
        .get("id")
        .and_then(Value::as_str)
        .expect("credential id")
        .to_owned();

    let svc = AntigravityService::in_memory_for_test(
        Some(store.clone()),
        AntigravityConfig::default(),
        AntigravityEndpoints::mock(&base),
    )
    .await
    .expect("svc");
    assert_eq!(svc.account_count(), 1);

    let creds = svc
        .credentials_for_account(0, true)
        .await
        .expect("refreshed credentials");
    assert_eq!(creds.access_token, "ya29.fresh");

    let row = store
        .get_credential(&cred_id)
        .await
        .expect("read credential")
        .expect("credential row");
    let value = row.get("value").expect("value");
    assert_eq!(
        value.get("access").and_then(Value::as_str),
        Some("ya29.fresh")
    );
    assert!(
        value.get("expires").and_then(Value::as_i64).unwrap_or(0) > 1,
        "expiry must be rewritten by the durable commit: {value}"
    );
    assert_eq!(
        value.pointer("/metadata/email").and_then(Value::as_str),
        Some("user@example.com"),
        "refresh must not drop credential metadata: {value}"
    );
    assert_eq!(
        value.pointer("/metadata/projectId").and_then(Value::as_str),
        Some("proj-1")
    );
    assert_eq!(
        value
            .pointer("/metadata/managedProjectId")
            .and_then(Value::as_str),
        Some("managed-9")
    );
    assert_eq!(
        value.get("methodID").and_then(Value::as_str),
        Some(ANTIGRAVITY_METHOD_ID)
    );
}

#[tokio::test]
async fn load_code_assist_tier_reaches_list_accounts_status() {
    let base = serve(
        Router::new()
            .route(
                "/v1internal:loadCodeAssist",
                post(|| async {
                    (
                        StatusCode::OK,
                        Json(json!({
                            "cloudaicompanionProject": { "id": "managed-42" },
                            "currentTier": { "id": "free-tier" }
                        })),
                    )
                }),
            )
            .route(
                "/v1internal:onboardUser",
                post(|| async {
                    (
                        StatusCode::OK,
                        Json(json!({ "done": true, "response": { "cloudaicompanionProject": { "id": "managed-42" } } })),
                    )
                }),
            ),
    )
    .await;

    let svc = AntigravityService::in_memory_for_test(
        None,
        AntigravityConfig::default(),
        AntigravityEndpoints::mock(&base),
    )
    .await
    .expect("svc");

    // No packed project id, so the project/tier resolution has to call `loadCodeAssist`.
    svc.upsert_exchange_result(
        AntigravityTokenExchange {
            refresh: "1//tok".to_owned(),
            refresh_token: "1//tok".to_owned(),
            access: "ya29.tok".to_owned(),
            expires: now_ms() + 3_600_000,
            email: Some("tier@example.com".to_owned()),
            project_id: String::new(),
            explicit_project_id: None,
            managed_project_id: None,
        },
        None,
    )
    .await
    .expect("account");

    let creds = svc.credentials_for_account(0, false).await.expect("creds");
    assert_eq!(creds.project_id, "managed-42");

    let accounts = svc.list_accounts_status().expect("status");
    assert_eq!(accounts[0].tier.as_deref(), Some("free-tier"));
    assert_eq!(accounts[0].project_id.as_deref(), Some("managed-42"));
}

#[tokio::test]
async fn background_maintenance_is_a_noop_when_disabled() {
    let svc = AntigravityService::in_memory_for_test(
        None,
        AntigravityConfig {
            proactive_token_refresh: false,
            quota_refresh_interval_minutes: 0,
            ..AntigravityConfig::default()
        },
        AntigravityEndpoints::default(),
    )
    .await
    .expect("svc");
    assert!(!svc.spawn_background_maintenance());
}

#[tokio::test]
async fn proactive_refresh_skips_tokens_outside_the_buffer() {
    let base = serve(Router::new().route(
        "/token",
        post(|| async {
            (
                StatusCode::OK,
                Json(json!({ "access_token": "ya29.fresh", "expires_in": 3600 })),
            )
        }),
    ))
    .await;

    let svc = AntigravityService::in_memory_for_test(
        None,
        AntigravityConfig::default(),
        AntigravityEndpoints::mock(&base),
    )
    .await
    .expect("svc");
    svc.upsert_exchange_result(
        AntigravityTokenExchange {
            refresh: "1//tok|proj-1".to_owned(),
            refresh_token: "1//tok".to_owned(),
            access: "ya29.valid".to_owned(),
            expires: now_ms() + 3_600_000,
            email: Some("fresh@example.com".to_owned()),
            project_id: "proj-1".to_owned(),
            explicit_project_id: Some("proj-1".to_owned()),
            managed_project_id: None,
        },
        None,
    )
    .await
    .expect("account");

    // Buffer far below remaining lifetime → nothing to refresh, no upstream hit.
    let refreshed = svc.proactive_refresh_tokens(60_000).await.expect("sweep");
    assert_eq!(refreshed, 0);

    // Buffer above remaining lifetime → the account is refreshed.
    let refreshed = svc
        .proactive_refresh_tokens(7_200_000)
        .await
        .expect("sweep");
    assert_eq!(refreshed, 1);
}
