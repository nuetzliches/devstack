//! The pool in the dashboard (ADR-0032): the hub calls the coordinator with
//! this machine's member certificate, so the browser never holds it, and the
//! coordinator decides by that member's role.
//!
//! - `GET /hub/pool`: this machine's membership, the members and nodes, and
//!   for an admin the members with their addresses and the audit log
//! - `POST /hub/pool/members/{id}/approve` `{role?}`, `.../remove`,
//!   `.../role` `{role}`, `POST /hub/pool/nodes/{id}/pause|resume`,
//!   `POST /hub/pool/leases/{id}/end`: an admin's changes, passed on as they
//!   are
//! - `POST /hub/pool/join` `{coordinator, fingerprint, name?}`: ask to join a
//!   pool (`devstack join` without waiting)

use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use devstack_node::member::{Local, MemberError};
use devstack_node::store::{Role, State};
use serde::Deserialize;
use serde_json::{Value, json};

fn local() -> Local {
    let state = devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf();
    Local::new(&devstack_node::member::default_dir(&state))
}

fn error(e: &MemberError) -> Response {
    let (status, msg) = match e {
        // The coordinator's own answer, with its status: 403 for a member
        // who is no admin, 409 for the last admin, 404 for an unknown id.
        MemberError::Status(code, msg) => (
            StatusCode::from_u16(*code).unwrap_or(StatusCode::BAD_GATEWAY),
            serde_json::from_str::<Value>(msg)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or_else(|| msg.clone()),
        ),
        MemberError::Http(e) => (
            StatusCode::BAD_GATEWAY,
            format!("the coordinator does not answer: {e}"),
        ),
        e => (StatusCode::BAD_GATEWAY, e.to_string()),
    };
    (status, Json(json!({ "error": msg }))).into_response()
}

/// Run a blocking call to the coordinator off the async runtime.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce(&Local) -> Result<T, MemberError> + Send + 'static,
) -> Result<T, MemberError> {
    tokio::task::spawn_blocking(move || f(&local()))
        .await
        .unwrap_or_else(|e| Err(MemberError::Other(format!("pool call failed: {e}"))))
}

/// Everything the pool area shows, in one answer. A machine in no pool, or
/// not approved yet, gets what it can see: how to join, or that it waits.
pub async fn overview() -> Response {
    let r = blocking(|l| {
        let Some(mut m) = l.membership()? else {
            return Ok(json!({ "joined": false }));
        };
        if m.state == State::Pending {
            m = l.refresh()?;
        }
        let me = json!({
            "id": m.id, "name": m.name, "state": m.state, "coordinator": m.coordinator,
        });
        if m.state != State::Approved {
            return Ok(json!({ "joined": true, "me": me }));
        }
        let mine = l.me()?;
        let mut v = json!({ "joined": true, "me": me, "nodes": l.nodes()?, "leases": l.leases()? });
        v["me"]["role"] = json!(mine.role);
        // What another machine runs to join this pool; the fingerprint is
        // public, the approval is what admits it.
        v["join"] = json!({
            "coordinator": m.coordinator,
            "fingerprint": m.ca_fingerprint,
            "command": format!("devstack join {} --fingerprint {}", m.coordinator, m.ca_fingerprint),
        });
        if mine.role == Role::Admin {
            v["members"] = json!(l.admin_members()?);
            v["audit"] = json!(l.admin_audit()?);
        } else {
            v["members"] = json!(l.members()?);
        }
        Ok(v)
    })
    .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(&e),
    }
}

#[derive(Deserialize, Default)]
pub struct RoleBody {
    #[serde(default)]
    role: Option<Role>,
}

pub async fn approve(Path(id): Path<String>, body: Option<Json<RoleBody>>) -> Response {
    let role = body.and_then(|Json(b)| b.role);
    match blocking(move |l| l.admin_approve(&id, role)).await {
        Ok(m) => Json(m).into_response(),
        Err(e) => error(&e),
    }
}

pub async fn remove(Path(id): Path<String>) -> Response {
    match blocking(move |l| l.admin_remove(&id)).await {
        Ok(m) => Json(m).into_response(),
        Err(e) => error(&e),
    }
}

pub async fn pause(Path(id): Path<String>) -> Response {
    match blocking(move |l| l.admin_pause(&id, true)).await {
        Ok(m) => Json(m).into_response(),
        Err(e) => error(&e),
    }
}

pub async fn resume(Path(id): Path<String>) -> Response {
    match blocking(move |l| l.admin_pause(&id, false)).await {
        Ok(m) => Json(m).into_response(),
        Err(e) => error(&e),
    }
}

pub async fn end(Path(id): Path<String>) -> Response {
    match blocking(move |l| l.admin_end(&id)).await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(e) => error(&e),
    }
}

#[derive(Deserialize)]
pub struct JoinBody {
    coordinator: String,
    fingerprint: String,
    #[serde(default)]
    name: Option<String>,
}

/// Ask a pool's coordinator to admit this machine; the answer comes with
/// the approval, which `GET /hub/pool` picks up.
pub async fn join(Json(b): Json<JoinBody>) -> Response {
    let name = b
        .name
        .filter(|n| !n.trim().is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "member".into());
    match blocking(move |l| l.join(b.coordinator.trim(), &b.fingerprint, name.trim())).await {
        Ok(m) => Json(json!({ "id": m.id, "name": m.name, "state": m.state })).into_response(),
        Err(e) => error(&e),
    }
}

pub async fn role(Path(id): Path<String>, Json(b): Json<RoleBody>) -> Response {
    let Some(role) = b.role else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "name the role: member or admin" })),
        )
            .into_response();
    };
    match blocking(move |l| l.admin_role(&id, role)).await {
        Ok(m) => Json(m).into_response(),
        Err(e) => error(&e),
    }
}
