//! SSE invalidations, never clinical records. A reconnect always requires a refetch.
use crate::api::{App, Error, actor, credential};
use axum::{
    extract::State,
    http::HeaderMap,
    response::sse::{Event, KeepAlive, Sse},
};
use sqlx::postgres::PgListener;
use std::{convert::Infallible, sync::Arc, time::Duration};
use tokio::sync::{OnceCell, Semaphore, broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use uuid::Uuid;

// One LISTEN connection per server process, not one per Windows workstation.
static STREAMS: Semaphore = Semaphore::const_new(64);
static HUB: OnceCell<broadcast::Sender<(Option<Uuid>, Option<u8>)>> = OnceCell::const_new();
async fn hub(app: Arc<App>) -> &'static broadcast::Sender<(Option<Uuid>, Option<u8>)> {
    HUB.get_or_init(|| async move {
        let (sender, _) = broadcast::channel(128);
        let output = sender.clone();
        tokio::spawn(async move {
            loop {
                if let Ok(mut listener) = PgListener::connect_with(&app.db).await
                    && listener
                        .listen_all(["m53_client_changes", "m53_update_changes", "m53_template_changes"])
                        .await
                        .is_ok()
                {
                    // Covers updates before LISTEN and after any database outage.
                    let _ = output.send((None, None));
                    let mut probe_at = tokio::time::Instant::now() + Duration::from_secs(5);
                    loop {
                        // LISTEN pins its connection, bypassing pool checkout
                        // checks. Recheck that same backend after writer failover,
                        // even while notifications keep the stream busy.
                        if tokio::time::Instant::now() >= probe_at {
                            let writable = tokio::time::timeout(Duration::from_secs(2),
                                sqlx::query_scalar::<_, bool>("SELECT NOT pg_is_in_recovery() AND current_setting('transaction_read_only')='off'")
                                    .fetch_one(&mut listener)).await;
                            if !matches!(writable, Ok(Ok(true))) { break; }
                            probe_at = tokio::time::Instant::now() + Duration::from_secs(5);
                        }
                        // SQLx buffers complete protocol messages; cancelling
                        // its notification wait preserves partial input bytes.
                        match tokio::time::timeout_at(probe_at, listener.try_recv()).await {
                            Ok(Ok(Some(notification))) => {
                                if let Ok(org) = notification.payload().parse() {
                                    let _ = output.send((
                                        Some(org),
                                        Some(match notification.channel() { "m53_update_changes" => 1, "m53_template_changes" => 2, _ => 0 }),
                                    ));
                                }
                            }
                            Ok(Ok(None)) => {
                                let _ = output.send((None, None));
                            }
                            Ok(Err(_)) => break,
                            Err(_) => continue,
                        }
                    }
                }
                let _ = output.send((None, None));
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        sender
    })
    .await
}
fn invalidation(reason: &str, updates: bool) -> Event {
    if updates {
        return Event::default()
            .event("client.update_available")
            .retry(Duration::from_secs(2))
            .data("{}");
    }
    Event::default()
        .event("refresh")
        .retry(Duration::from_secs(2))
        .data(serde_json::json!({"reason":reason,"resources":["tasks","patients","studies","deliveries"]}).to_string())
}
pub async fn subscribe(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, Error> {
    subscribe_inner(app, headers, false).await
}
pub async fn workstation_subscribe(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, Error> {
    subscribe_inner(app, headers, true).await
}
async fn subscribe_inner(
    app: Arc<App>,
    headers: HeaderMap,
    updates: bool,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, Error> {
    let org = if updates {
        credential(&app, &headers).await?.1
    } else {
        actor(&app, &headers).await?.org
    };
    let permit = STREAMS.try_acquire().map_err(|_| {
        Error(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "event_stream_limit",
        )
    })?;
    let mut changes = hub(app.clone()).await.subscribe();
    let (sender, receiver) = mpsc::channel(4);
    sender.try_send(Ok(invalidation("connected", updates))).ok();
    tokio::spawn(async move {
        let _permit = permit;
        let mut timer = tokio::time::interval(Duration::from_secs(5));
        loop {
            let reason = tokio::select! {
                _ = sender.closed() => break,
                _ = timer.tick() => None,
                change = changes.recv() => match change {
                    Ok((Some(changed),_)) if changed != org => continue,
                    Ok((_,Some(kind))) if (kind == 1) != updates => continue,
                    Ok((_,Some(2))) => Some("templates-updated"),
                    Ok(_) => Some("changed"),
                    Err(broadcast::error::RecvError::Lagged(_)) => Some("resync"),
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            };
            // Only a confirmed authorization refusal revokes the client session.
            // A database/transport failure closes the stream for normal reconnect.
            let check = async {
                if updates {
                    credential(&app, &headers).await.map(|_| ())
                } else {
                    actor(&app, &headers).await.map(|_| ())
                }
            };
            // This task outlives creation of the HTTP response. Bound its DB
            // wait too, and release the stream slot when the consumer leaves.
            let access = tokio::select! {
                _ = sender.closed() => break,
                result = tokio::time::timeout(Duration::from_secs(5), check) => match result {
                    Ok(access) => access,
                    Err(_) => break,
                },
            };
            if let Err(error) = access {
                if matches!(error.0, axum::http::StatusCode::UNAUTHORIZED | axum::http::StatusCode::FORBIDDEN) {
                    let _ = sender.try_send(Ok(Event::default().event("auth-required").data("{}")));
                }
                break;
            }
            if let Some(reason) = reason
                && sender.try_send(Ok(if reason == "templates-updated" { Event::default().event("templates-updated").data("{}") } else { invalidation(reason, updates) })).is_err()
            {
                // A slow consumer reconnects and refetches; memory stays bounded.
                break;
            }
        }
    });
    Ok(Sse::new(ReceiverStream::new(receiver)).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(10))
            .text("keepalive"),
    ))
}
