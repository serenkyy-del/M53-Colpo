use crate::api::hash;
use m53_domain::{protocol::Region, validation::TermKind};
use serde::Deserialize;
use sqlx::Row;
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    version_id: Uuid,
    code: String,
    source: String,
    terms: Vec<CatalogTerm>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogTerm {
    code: String,
    region: Region,
    kind: TermKind,
    english: String,
    russian: String,
    source_locator: String,
}
pub async fn import(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let catalog: Catalog = serde_json::from_slice(&bytes)?;
    if catalog.version_id.is_nil()
        || catalog.terms.is_empty()
        || catalog.code.trim().is_empty()
        || catalog.source.trim().is_empty()
    {
        return Err("Invalid dictionary".into());
    }
    let mut codes = std::collections::BTreeSet::new();
    for term in &catalog.terms {
        if !codes.insert(&term.code)
            || term.code.trim().is_empty()
            || term.english.trim().is_empty()
            || term.source_locator.trim().is_empty()
        {
            return Err("Duplicate or incomplete dictionary term".into());
        }
    }
    let db = crate::deployment::connect(crate::deployment::Purpose::Migrator).await?;
    let digest = hash(&bytes);
    let mut tx = db.begin().await?;
    sqlx::query("SET LOCAL ROLE colposcopy_owner")
        .execute(&mut *tx)
        .await?;
    if let Some(row) =
        sqlx::query("SELECT content_sha256 FROM app.terminology_versions WHERE id=$1")
            .bind(catalog.version_id)
            .fetch_optional(&mut *tx)
            .await?
    {
        if row.get::<String, _>("content_sha256") == digest {
            return Ok(());
        }
        return Err(
            "Dictionary version exists with different content; create a new version".into(),
        );
    }
    sqlx::query("INSERT INTO app.terminology_versions(id,code,source,content_sha256,clinically_approved) VALUES($1,$2,$3,$4,false)").bind(catalog.version_id).bind(catalog.code).bind(catalog.source).bind(digest).execute(&mut *tx).await?;
    for term in catalog.terms {
        let region = serde_json::to_value(term.region)?
            .as_str()
            .unwrap()
            .to_string();
        let kind = serde_json::to_value(term.kind)?
            .as_str()
            .unwrap()
            .to_string();
        sqlx::query("INSERT INTO app.terminology_terms(version_id,code,region,kind,english,russian,source_locator) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(catalog.version_id).bind(term.code).bind(region).bind(kind).bind(term.english).bind(term.russian).bind(term.source_locator).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    eprintln!("Draft terminology imported; clinical approval remains false");
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClinicalReview { version_id: Uuid, content_sha256: String }
fn clinical_review(bytes:&[u8])->Result<ClinicalReview,Box<dyn std::error::Error>> {
 let review:ClinicalReview=serde_json::from_slice(bytes)?;
 if review.version_id.is_nil() || review.content_sha256.len()!=64 || !review.content_sha256.bytes().all(|c|c.is_ascii_digit()||(b'a'..=b'f').contains(&c)){return Err("invalid_clinical_review".into())}
 Ok(review)
}
/// Operator authorization of one immutable imported dictionary and exact digest.
pub async fn approve_review(path:&str)->Result<(),Box<dyn std::error::Error>> {
 let review=clinical_review(&std::fs::read(path)?)?;
 let db=crate::deployment::connect(crate::deployment::Purpose::Migrator).await?;
 let mut tx=db.begin().await?;sqlx::query("SET LOCAL ROLE colposcopy_owner").execute(&mut *tx).await?;
 let result=sqlx::query("UPDATE app.terminology_versions SET clinically_approved=true WHERE id=$1 AND content_sha256=$2").bind(review.version_id).bind(review.content_sha256).execute(&mut *tx).await?;
 if result.rows_affected()!=1{return Err("review_dictionary_digest_mismatch".into())}
 tx.commit().await?;Ok(())
}
#[cfg(test)] mod review_tests {
 #[test] fn rejects_review_without_exact_digest(){assert!(super::clinical_review(br#"{"version_id":"00000000-0000-0000-0000-000000000001","content_sha256":"incorrect"}"#).is_err());}
}
