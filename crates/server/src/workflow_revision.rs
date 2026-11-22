//! A locking SELECT may have started its snapshot before a concurrent reopen.
//! Validate the revision in a fresh statement after obtaining the study lock.
use crate::api::Error;
use axum::http::StatusCode;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

pub(crate) async fn require_current_approved(
    tx: &mut Transaction<'_, Postgres>,
    study: Uuid,
    revision: Uuid,
) -> Result<(), Error> {
    sqlx::query("SELECT id FROM app.studies WHERE id=$1 FOR UPDATE")
        .bind(study).fetch_one(&mut **tx).await?;
    let current = sqlx::query("SELECT s.status,r.id,r.approved_at IS NOT NULL AS approved FROM app.studies s JOIN LATERAL (SELECT id,approved_at FROM app.protocol_revisions WHERE study_id=s.id ORDER BY revision DESC LIMIT 1) r ON true WHERE s.id=$1")
        .bind(study).fetch_optional(&mut **tx).await?;
    if current.is_some_and(|row| row.get::<String,_>("status")=="approved" && row.get::<Uuid,_>("id")==revision && row.get::<bool,_>("approved")) {
        Ok(())
    } else {
        Err(Error(StatusCode::CONFLICT,"approved_report_required"))
    }
}
