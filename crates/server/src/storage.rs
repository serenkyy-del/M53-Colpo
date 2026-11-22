//! Crash recovery inventory: ready assets are verified; recent uncommitted files are left alone.
use crate::{api::hash, setup};
use sqlx::Row;
use std::{collections::BTreeSet, env, path::PathBuf};
pub async fn reconcile(cleanup: bool) -> Result<(), Box<dyn std::error::Error>> {
    let db = setup::connect().await?;
    let root = PathBuf::from(env::var("M53_STORAGE_DIR")?);
    if !root.is_absolute() {
        return Err("Storage root must be absolute".into());
    }
    let rows = sqlx::query("SELECT id,storage_key,sha256,byte_size FROM app.assets")
        .fetch_all(&db)
        .await?;
    let mut keys = BTreeSet::new();
    let mut invalid = 0;
    let mut orphans = 0;
    for row in rows {
        let key: String = row.get("storage_key");
        let id: uuid::Uuid = row.get("id");
        if key != id.simple().to_string() {
            invalid += 1;
            continue;
        }
        keys.insert(key.clone());
        match tokio::fs::read(root.join(&key)).await {
            Ok(bytes)
                if hash(&bytes) == row.get::<String, _>("sha256")
                    && bytes.len() as i64 == row.get::<i64, _>("byte_size") => {}
            _ => invalid += 1,
        }
    }
    let mut entries = tokio::fs::read_dir(&root).await?;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !keys.contains(&name) {
            orphans += 1;
            let age = entry
                .metadata()
                .await?
                .modified()?
                .elapsed()
                .unwrap_or_default();
            if cleanup && age.as_secs() > 86400 {
                tokio::fs::remove_file(entry.path()).await?;
            }
        }
    }
    println!("{{\"invalid_ready_assets\":{invalid},\"uncommitted_files\":{orphans}}}");
    if invalid > 0 {
        return Err("Storage integrity check failed".into());
    }
    Ok(())
}

/// Data reaches stable storage before the database can expose the ready asset.
/// Publishing never replaces an existing final path, even for a reused asset ID.
pub async fn publish(root: &std::path::Path, key: &str, bytes: &[u8]) -> std::io::Result<()> {
    publish_with_temp(root, key, bytes, &format!("{key}.part")).await
}

/// Retry an upload whose file publication may have outlived a rolled-back DB
/// transaction. Never replace bytes: only an exact, regular final file can be
/// adopted. The caller serializes the asset UUID and verifies DB ownership.
pub async fn publish_recoverable(
    root: &std::path::Path,
    key: &str,
    bytes: &[u8],
) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    // A crashed writer may leave its .part behind. Each new attempt owns a new
    // path, so recovery neither waits for nor unlinks another writer's partial.
    let temp_name = format!("{key}.{}.part", uuid::Uuid::new_v4().simple());
    match publish_with_temp(root, key, bytes, &temp_name).await {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let conflict = || {
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "Existing asset is not the exact uploaded file",
                )
            };
            let path = root.join(key);
            let metadata = tokio::fs::symlink_metadata(&path).await?;
            if !metadata.file_type().is_file() || metadata.len() != bytes.len() as u64 {
                return Err(conflict());
            }
            let mut file = tokio::fs::File::open(&path).await?;
            let metadata = file.metadata().await?;
            if !metadata.is_file() || metadata.len() != bytes.len() as u64 {
                return Err(conflict());
            }
            let mut buffer = [0u8; 64 * 1024];
            for expected in bytes.chunks(buffer.len()) {
                match file.read_exact(&mut buffer[..expected.len()]).await {
                    Ok(_) if buffer[..expected.len()] == *expected => {}
                    Ok(_) => return Err(conflict()),
                    Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                        return Err(conflict());
                    }
                    Err(error) => return Err(error),
                }
            }
            if file.read(&mut buffer[..1]).await? != 0 {
                return Err(conflict());
            }
            // The earlier writer could have failed after link but before its
            // final directory fsync. Re-establish durability before any ACK.
            file.sync_all().await?;
            #[cfg(unix)]
            tokio::fs::File::open(root).await?.sync_all().await?;
            Ok(())
        }
        result => result,
    }
}

async fn publish_with_temp(
    root: &std::path::Path,
    key: &str,
    bytes: &[u8],
    temp_name: &str,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    if key.len() != 32 || !key.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Invalid storage key",
        ));
    }
    let temp = root.join(temp_name);
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp).await?;
    let mut owns_temp = true;
    // Only the writer which won create_new owns this temporary path. If opening
    // fails, leave another writer's .part untouched.
    let result = async {
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        // Same-filesystem hard_link atomically creates final or fails AlreadyExists.
        // Unlike rename, it cannot overwrite original bytes before a DB conflict.
        tokio::fs::hard_link(&temp, root.join(key)).await?;
        // Do not retry unlink after publication: another writer may create a new
        // .part after this one is removed, before the directory fsync completes.
        owns_temp = false;
        tokio::fs::remove_file(&temp).await?;
        #[cfg(unix)]
        tokio::fs::File::open(root).await?.sync_all().await?;
        Ok(())
    }
    .await;
    if result.is_err() && owns_temp {
        let _ = tokio::fs::remove_file(&temp).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{publish, publish_recoverable};
    use std::io::ErrorKind;
    use uuid::Uuid;

    async fn directory() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("m53-storage-unit-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&path).await.unwrap();
        path
    }

    #[tokio::test]
    async fn reused_asset_id_cannot_replace_existing_original_bytes() {
        let root = directory().await;
        let key = Uuid::new_v4().simple().to_string();
        let original = b"immutable original bytes";
        publish(&root, &key, original).await.unwrap();
        assert_eq!(
            publish(&root, &key, b"different replacement bytes")
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(tokio::fs::read(root.join(&key)).await.unwrap(), original);
        assert!(!root.join(format!("{key}.part")).exists());
        assert_eq!(
            publish(&root, &key, original).await.unwrap_err().kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(tokio::fs::read(root.join(&key)).await.unwrap(), original);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn losing_temp_creation_preserves_the_other_writers_partial_file() {
        let root = directory().await;
        let key = Uuid::new_v4().simple().to_string();
        let partial = root.join(format!("{key}.part"));
        tokio::fs::write(&partial, b"another writer owns this partial")
            .await
            .unwrap();
        assert_eq!(
            publish(&root, &key, b"replacement")
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(
            tokio::fs::read(partial).await.unwrap(),
            b"another writer owns this partial"
        );
        assert!(!root.join(key).exists());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn simultaneous_publish_has_one_winner_and_leaves_its_bytes_untouched() {
        let root = directory().await;
        let key = Uuid::new_v4().simple().to_string();
        let a = b"original candidate A";
        let b = b"original candidate B";
        let (first, second) = tokio::join!(publish(&root, &key, a), publish(&root, &key, b));
        assert_ne!(first.is_ok(), second.is_ok());
        let expected = if first.is_ok() { a } else { b };
        let loser = if first.is_ok() { second } else { first };
        assert_eq!(loser.unwrap_err().kind(), ErrorKind::AlreadyExists);
        assert_eq!(tokio::fs::read(root.join(&key)).await.unwrap(), expected);
        assert!(!root.join(format!("{key}.part")).exists());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn exact_orphan_can_be_recovered_without_replacing_its_inode() {
        let root = directory().await;
        let key = Uuid::new_v4().simple().to_string();
        publish(&root, &key, b"original after DB rollback")
            .await
            .unwrap();
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(root.join(&key)).unwrap().ino()
        };
        publish_recoverable(&root, &key, b"original after DB rollback")
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(root.join(&key)).await.unwrap(),
            b"original after DB rollback"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(root.join(&key)).unwrap().ino(), inode);
        }
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn recovery_rejects_different_equal_length_bytes_and_preserves_partials() {
        let root = directory().await;
        let key = Uuid::new_v4().simple().to_string();
        publish(&root, &key, b"original A").await.unwrap();
        let partial = root.join(format!("{key}.part"));
        tokio::fs::write(&partial, b"crashed writer").await.unwrap();
        assert_eq!(
            publish_recoverable(&root, &key, b"original B")
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::AlreadyExists
        );
        publish_recoverable(&root, &key, b"original A")
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(root.join(&key)).await.unwrap(),
            b"original A"
        );
        assert_eq!(tokio::fs::read(partial).await.unwrap(), b"crashed writer");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn crash_before_final_link_does_not_block_a_new_attempt() {
        let root = directory().await;
        let key = Uuid::new_v4().simple().to_string();
        let partial = root.join(format!("{key}.part"));
        tokio::fs::write(&partial, b"incomplete earlier upload")
            .await
            .unwrap();
        publish_recoverable(&root, &key, b"complete original")
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(root.join(&key)).await.unwrap(),
            b"complete original"
        );
        assert_eq!(
            tokio::fs::read(partial).await.unwrap(),
            b"incomplete earlier upload"
        );
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn recovery_never_adopts_a_symlink_even_for_identical_bytes() {
        let root = directory().await;
        let key = Uuid::new_v4().simple().to_string();
        let target = root.join("unrelated-original");
        tokio::fs::write(&target, b"same bytes").await.unwrap();
        std::os::unix::fs::symlink(&target, root.join(&key)).unwrap();
        assert_eq!(
            publish_recoverable(&root, &key, b"same bytes")
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::AlreadyExists
        );
        assert_eq!(tokio::fs::read(target).await.unwrap(), b"same bytes");
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
