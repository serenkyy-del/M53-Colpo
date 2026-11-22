//! Offline, transactional rotation. Stop API/worker and back up DB + old key first.
use crate::setup;
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use sqlx::Row;
use uuid::Uuid;
pub async fn rotate() -> Result<(), Box<dyn std::error::Error>> {
    let old = std::fs::read(std::env::var("M53_MASTER_KEY_FILE")?)?;
    let new = std::fs::read(std::env::var("M53_NEW_MASTER_KEY_FILE")?)?;
    if old.len() != 32 || new.len() != 32 || old == new {
        return Err("Two distinct 32-byte keys are required".into());
    }
    let key_id = std::env::var("M53_NEW_MASTER_KEY_ID")?;
    if key_id.is_empty() || key_id.len() > 128 {
        return Err("Invalid new key ID".into());
    }
    let old = Aes256Gcm::new_from_slice(&old).map_err(|_| "Invalid old key")?;
    let new = Aes256Gcm::new_from_slice(&new).map_err(|_| "Invalid new key")?;
    let db = setup::connect().await?;
    let mut tx = db.begin().await?;
    let rows = sqlx::query(
        "SELECT integration_id,nonce,ciphertext,key_id FROM app.integration_secrets FOR UPDATE",
    )
    .fetch_all(&mut *tx)
    .await?;
    for row in rows {
        if row.get::<String, _>("key_id") == key_id {
            return Err("New key ID is already in use".into());
        }
        let id: Uuid = row.get("integration_id");
        let nonce: Vec<u8> = row.get("nonce");
        let ciphertext: Vec<u8> = row.get("ciphertext");
        if nonce.len() != 12 {
            return Err("Invalid nonce".into());
        }
        let plaintext = old
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: id.as_bytes(),
                },
            )
            .map_err(|_| "Old key failed authentication; transaction rolled back")?;
        let random = Uuid::new_v4();
        let nonce = &random.as_bytes()[..12];
        let ciphertext = new
            .encrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: &plaintext,
                    aad: id.as_bytes(),
                },
            )
            .map_err(|_| "Encryption failed")?;
        sqlx::query("UPDATE app.integration_secrets SET key_id=$2,nonce=$3,ciphertext=$4 WHERE integration_id=$1").bind(id).bind(&key_id).bind(nonce).bind(ciphertext).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    eprintln!(
        "Rotation committed; switch the bootstrap key file and key ID before restarting API/worker"
    );
    Ok(())
}
