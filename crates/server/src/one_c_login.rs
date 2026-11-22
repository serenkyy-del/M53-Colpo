//! A mapped 1C password is verified directly; it is never stored in PostgreSQL.
use crate::{
    api::{self, App, Error},
    one_c_odata_auth::{OdataAuthOutcome, OneCOdataAuth},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
};
use m53_contracts::{
    one_c_access::{OneCLoginRequest, OneCLoginResponse},
    workflow::Session,
};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

pub async fn login(
    State(app): State<Arc<App>>,
    h: HeaderMap,
    Json(input): Json<OneCLoginRequest>,
) -> Result<Json<OneCLoginResponse>, Error> {
    let (credential, org) = api::credential(&app, &h).await?;
    if !input.valid() {
        return Err(Error(StatusCode::BAD_REQUEST, "invalid_1c_login_request"));
    }
    if std::env::var("M53_AUTH_PROVIDER").as_deref() != Ok("1c_web") {
        return Err(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "one_c_login_not_configured",
        ));
    }
    let row=sqlx::query("SELECT b.login,b.row_version,b.source_id,b.user_guid FROM app.doctor_1c_logins b JOIN app.doctors d ON d.id=b.doctor_id AND d.organization_id=b.organization_id JOIN app.integration_sources s ON s.id=b.source_id WHERE b.doctor_id=$1 AND b.organization_id=$2 AND b.enabled AND d.active AND s.enabled")
        .bind(input.doctor_id).bind(org).fetch_optional(&app.db).await?
        .ok_or(Error(StatusCode::FORBIDDEN,"doctor_1c_login_unavailable"))?;
    if row.get::<i64, _>("row_version") != input.binding_version {
        return Err(Error(StatusCode::CONFLICT, "login_profile_changed"));
    }
    let login: String = row.get("login");
    let key = api::hash(format!("1c_web:{}", input.doctor_id));
    let mut tx = app.db.begin().await?;
    sqlx::query("INSERT INTO app.login_attempts(credential_id,login_hash) VALUES($1,$2) ON CONFLICT DO NOTHING")
        .bind(credential).bind(&key).execute(&mut *tx).await?;
    let blocked:bool=sqlx::query_scalar("SELECT coalesce(blocked_until>now(),false) FROM app.login_attempts WHERE credential_id=$1 AND login_hash=$2 FOR UPDATE")
        .bind(credential).bind(&key).fetch_one(&mut *tx).await?;
    if blocked {
        return Err(Error(
            StatusCode::TOO_MANY_REQUESTS,
            "login_temporarily_blocked",
        ));
    }
    tx.commit().await?;
    let transport = OneCOdataAuth::from_env()
        .map_err(|_| Error(StatusCode::SERVICE_UNAVAILABLE, "one_c_login_configuration"))?
        .ok_or(Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "one_c_login_not_configured",
        ))?;
    // No database transaction or UI thread waits for 1C. The verified outcome
    // requires a Basic challenge and actual valid Edmx, not a generic HTTP200.
    match transport.check_credentials(&login, &input.password).await {
        OdataAuthOutcome::ExplicitDenied => {
            sqlx::query("UPDATE app.login_attempts SET failures=failures+1,blocked_until=CASE WHEN failures>=4 THEN now()+interval '5 minutes' ELSE NULL END WHERE credential_id=$1 AND login_hash=$2")
                .bind(credential).bind(&key).execute(&app.db).await?;
            return Err(Error(StatusCode::UNAUTHORIZED, "one_c_login_denied"));
        }
        OdataAuthOutcome::Unavailable(_) => {
            return Err(Error(
                StatusCode::SERVICE_UNAVAILABLE,
                "one_c_login_unavailable",
            ));
        }
        OdataAuthOutcome::Verified => {}
    }
    let mut tx = app.db.begin().await?;
    sqlx::query("SELECT c.id FROM app.api_credentials c JOIN app.workstations w ON w.id=c.workstation_id WHERE c.id=$1 AND w.organization_id=$2 AND c.purpose='client' AND c.revoked_at IS NULL AND (c.expires_at IS NULL OR c.expires_at>clock_timestamp()) AND 'clinical'=ANY(c.permissions) FOR SHARE OF c,w")
        .bind(credential).bind(org).fetch_optional(&mut *tx).await?
        .ok_or(Error(StatusCode::UNAUTHORIZED,"invalid_workstation"))?;
    let user: Uuid = sqlx::query_scalar("SELECT app.ensure_1c_principal($1,$2,$3,$4,$5)")
        .bind(input.doctor_id)
        .bind(org)
        .bind(row.get::<Uuid, _>("source_id"))
        .bind(row.get::<Uuid, _>("user_guid"))
        .bind(input.binding_version)
        .fetch_one(&mut *tx)
        .await
        .map_err(principal_error)?;
    let name: String = sqlx::query_scalar("SELECT display_name FROM app.users WHERE id=$1")
        .bind(user)
        .fetch_one(&mut *tx)
        .await?;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query("INSERT INTO app.user_sessions(token_hash,user_id,credential_id,expires_at,auth_provider,external_verified_at) VALUES($1,$2,$3,now()+interval '8 hours','1c_web',clock_timestamp())")
        .bind(api::hash(&token)).bind(user).bind(credential).execute(&mut *tx).await?;
    sqlx::query("UPDATE app.login_attempts SET failures=0,blocked_until=NULL WHERE credential_id=$1 AND login_hash=$2")
        .bind(credential).bind(&key).execute(&mut *tx).await?;
    api::audit(
        &mut tx,
        &api::Actor {
            session_hash: api::hash(&token),
            user,
            org,
            credential,
            request_id: h
                .get("x-m53-request-id")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| Uuid::parse_str(v).ok()),
            operation_id: None,
        },
        user,
        "user.login_1c",
        1,
    )
    .await?;
    tx.commit().await?;
    let mut authenticated = h;
    authenticated.insert(
        header::AUTHORIZATION,
        format!("Bearer {token}")
            .parse()
            .map_err(|_| Error(StatusCode::INTERNAL_SERVER_ERROR, "session_encoding_failed"))?,
    );
    let prepared = async {
        let Json(me) = crate::auth_access::me(State(app.clone()), authenticated.clone()).await?;
        let Json(offline_grant) =
            crate::auth_access::offline_grant(State(app.clone()), authenticated).await?;
        Ok::<_, Error>((me, offline_grant))
    }
    .await;
    let (me, offline_grant) = match prepared {
        Ok(value) => value,
        Err(error) => {
            // Never return a partial login without the required local-unlock
            // preparation. A later retry performs a fresh 1C verification.
            let _ = sqlx::query(
                "UPDATE app.user_sessions SET revoked_at=clock_timestamp() WHERE token_hash=$1",
            )
            .bind(api::hash(&token))
            .execute(&app.db)
            .await;
            return Err(error);
        }
    };
    Ok(Json(OneCLoginResponse {
        auth_method: "odata_basic".into(),
        session: Session {
            user_id: user,
            token,
            display_name: name,
        },
        me,
        offline_grant,
    }))
}

fn principal_error(error: sqlx::Error) -> Error {
    if let sqlx::Error::Database(ref db) = error {
        match db.code().as_deref() {
            Some("42501") => return Error(StatusCode::FORBIDDEN, "one_c_principal_denied"),
            Some("40001") => return Error(StatusCode::CONFLICT, "login_profile_changed"),
            _ => {}
        }
    }
    error.into()
}
