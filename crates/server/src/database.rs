//! Bounded connections to one writable PostgreSQL endpoint, including primary/standby VIPs.
//! Promotion, fencing and replication configuration belong to the cluster operator.
use axum::{
    Json,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
use sqlx::{
    Connection, PgConnection, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{env, str::FromStr, sync::Arc, time::Duration};

#[derive(Clone, Debug)]
pub(crate) struct Policy {
    max_connections: u32,
    acquire: Duration,
    statement_ms: u64,
    lock_ms: u64,
    idle_transaction_ms: u64,
    pub(crate) deadline: Duration,
    upload_deadline: Duration,
    ack: Duration,
    sync_replicas: i64,
}

fn setting(name: &str, default: u64, minimum: u64, maximum: u64) -> Result<u64, String> {
    let value = env::var(name)
        .ok()
        .map(|v| v.parse::<u64>())
        .transpose()
        .map_err(|_| format!("invalid_{name}"))?
        .unwrap_or(default);
    if !(minimum..=maximum).contains(&value) {
        return Err(format!("invalid_{name}"));
    }
    Ok(value)
}

impl Policy {
    pub(crate) fn from_env() -> Result<Self, String> {
        Ok(Self {
            max_connections: setting("M53_DB_MAX_CONNECTIONS", 8, 2, 128)? as u32,
            acquire: Duration::from_millis(setting("M53_DB_ACQUIRE_TIMEOUT_MS", 3000, 100, 60000)?),
            statement_ms: setting("M53_DB_STATEMENT_TIMEOUT_MS", 15000, 100, 120000)?,
            lock_ms: setting("M53_DB_LOCK_TIMEOUT_MS", 4000, 100, 120000)?,
            idle_transaction_ms: setting(
                "M53_DB_IDLE_TRANSACTION_TIMEOUT_MS",
                30000,
                1000,
                300000,
            )?,
            deadline: Duration::from_millis(setting("M53_HTTP_DEADLINE_MS", 30000, 100, 120000)?),
            upload_deadline: Duration::from_millis(setting(
                "M53_HTTP_UPLOAD_DEADLINE_MS",
                90000,
                100,
                180000,
            )?),
            ack: Duration::from_millis(setting("M53_DB_ACK_TIMEOUT_MS", 3000, 100, 60000)?),
            sync_replicas: setting("M53_DB_SYNC_REPLICAS", 0, 0, 16)? as i64,
        })
    }
}

async fn writable(connection: &mut PgConnection) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT NOT pg_is_in_recovery() AND current_setting('transaction_read_only')='off'",
    )
    .fetch_one(connection)
    .await
}

// The timeline changes on promotion. Instance identity also detects reconnects
// to a different primary even before its first checkpoint, without a second
// pool slot held throughout the HTTP handler.
const IDENTITY: &str = "SELECT json_build_array(pg_postmaster_start_time(),inet_server_addr(),inet_server_port(),left(pg_walfile_name(pg_current_wal_lsn()),8))::text";

async fn identity(db: &PgPool) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(IDENTITY).fetch_one(db).await
}

pub(crate) async fn connect(url: &str) -> Result<PgPool, Box<dyn std::error::Error>> {
    let policy = Policy::from_env()?;
    let options = PgConnectOptions::from_str(url)?
        .application_name("m53-api")
        .options([
            ("statement_timeout", policy.statement_ms.to_string()),
            ("lock_timeout", policy.lock_ms.to_string()),
            (
                "idle_in_transaction_session_timeout",
                policy.idle_transaction_ms.to_string(),
            ),
            ("synchronous_commit", "on".to_owned()),
            ("tcp_keepalives_idle", "10".to_owned()),
            ("tcp_keepalives_interval", "3".to_owned()),
            ("tcp_keepalives_count", "3".to_owned()),
            ("tcp_user_timeout", "10000".to_owned()),
        ]);
    let sync_replicas = policy.sync_replicas;
    let release_timeout = policy.acquire;
    let pool = PgPoolOptions::new().max_connections(policy.max_connections)
        .acquire_timeout(policy.acquire)
        .max_lifetime(Duration::from_secs(300))
        .idle_timeout(Duration::from_secs(60))
        // Replace the default ping with one role/liveness check, not two round trips.
        .test_before_acquire(false)
        .before_acquire(|connection, _| Box::pin(writable(connection)))
        // SQLx otherwise waits without a deadline for a pending cancelled query
        // while returning a connection. Discard it if draining the wire stalls.
        .after_release(move |connection, _| Box::pin(async move {
            match tokio::time::timeout(release_timeout, connection.ping()).await {
                Ok(result) => result.map(|_| true),
                Err(_) => Err(sqlx::Error::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "database_release_timeout"))),
            }
        }))
        .after_connect(move |connection, _| Box::pin(async move {
            if !writable(connection).await? {
                return Err(sqlx::Error::Protocol("database_not_writable_primary".into()));
            }
            if sync_replicas > 0 {
                let configured: bool = sqlx::query_scalar("SELECT current_setting('synchronous_commit')='on' AND current_setting('synchronous_standby_names')<>'' AND pg_has_role(current_user,'pg_read_all_stats','USAGE')")
                    .fetch_one(connection).await?;
                if !configured { return Err(sqlx::Error::Protocol("synchronous_replication_required".into())); }
            }
            Ok(())
        }))
        .connect_with(options).await?;
    Ok(pool)
}

/// An ACK after an uncertain COMMIT also needs replication proof: the receipt
/// may be visible on the primary after its synchronous commit wait was cancelled.
/// Capture a WAL position after the handler, then wait for remote durable flush.
async fn replicated(db: &PgPool, policy: &Policy, origin: Option<&str>) -> Result<(), sqlx::Error> {
    let mut connection = db.acquire().await?;
    if policy.sync_replicas == 0 {
        return Ok(());
    }
    let current: String = sqlx::query_scalar(IDENTITY)
        .fetch_one(&mut *connection)
        .await?;
    if origin.is_some_and(|origin| origin != current) {
        return Err(sqlx::Error::Protocol("database_primary_changed".into()));
    }
    let target: String = sqlx::query_scalar("SELECT pg_current_wal_flush_lsn()::text")
        .fetch_one(&mut *connection)
        .await?;
    loop {
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_replication WHERE state='streaming' AND sync_state IN ('sync','quorum') AND flush_lsn >= $1::text::pg_lsn")
            .bind(&target).fetch_one(&mut *connection).await?;
        if count >= policy.sync_replicas {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub(crate) async fn ready(db: &PgPool, policy: &Policy) -> bool {
    matches!(
        tokio::time::timeout(policy.ack, replicated(db, policy, None)).await,
        Ok(Ok(()))
    )
}

#[derive(Clone)]
pub(crate) struct Runtime {
    pub(crate) db: PgPool,
    pub(crate) policy: Arc<Policy>,
}

pub(crate) async fn deadline(
    State(runtime): State<Runtime>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if path == "/health" {
        return next.run(request).await;
    }
    let needs_ack = path.starts_with("/api/")
        && !matches!(path, "/api/v1/events" | "/api/v1/workstation-events");
    // File bodies have their existing larger client transfer budget. Their
    // upload wait must not inherit the shorter metadata/transaction deadline.
    let budget = if request.method() == axum::http::Method::POST
        && (path.ends_with("/assets") || path.ends_with("/results/1c/pdf"))
    {
        runtime.policy.upload_deadline
    } else {
        runtime.policy.deadline
    };
    let work = async {
        let origin = if needs_ack && runtime.policy.sync_replicas > 0 {
            match identity(&runtime.db).await {
                Ok(identity) => Some(identity),
                Err(_) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"code":"database_not_ready"})),
                    )
                        .into_response();
                }
            }
        } else {
            None
        };
        let response = next.run(request).await;
        if needs_ack
            && response.status().is_success()
            && runtime.policy.sync_replicas > 0
            && !matches!(
                tokio::time::timeout(
                    runtime.policy.ack,
                    replicated(&runtime.db, &runtime.policy, origin.as_deref())
                )
                .await,
                Ok(Ok(()))
            )
        {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"code":"database_ack_pending"})),
            )
                .into_response();
        }
        response
    };
    tokio::time::timeout(budget, work)
        .await
        .unwrap_or_else(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"code":"database_request_timeout"})),
            )
                .into_response()
        })
}
