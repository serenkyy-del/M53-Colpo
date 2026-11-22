//! Administrator-controlled 1C base, shared by authentication and PDF delivery.
pub(crate) fn endpoint(base: &str, suffix: &str) -> Option<String> {
    let url = reqwest::Url::parse(base).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str() != base
        || !url.path().ends_with('/')
        || url.path().trim_matches('/').is_empty()
        || url.path().contains("//")
        || base.contains(['%', '\\'])
        || !matches!(suffix, "odata/standard.odata/$metadata" | "hs/reception/colposcopy-result")
    {
        return None;
    }
    Some(format!("{base}{suffix}"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_host_changes_without_code_changes() {
        for host in ["192.168.73.45", "192.168.73.46", "one-c.example.test"] {
            let base = format!("http://{host}:885/UMZ_BIT-DEV/");
            assert_eq!(endpoint(&base, "odata/standard.odata/$metadata"), Some(format!("{base}odata/standard.odata/$metadata")));
            assert_eq!(endpoint(&base, "hs/reception/colposcopy-result"), Some(format!("{base}hs/reception/colposcopy-result")));
        }
    }
    #[test]
    fn base_rejects_ambiguous_or_credential_bearing_urls() {
        for base in ["http://u:p@host:885/IB/", "http://host:885/IB/?x=y", "http://host:885/IB/#f", "http://host:885/", "http://host:885/IB", "http://host:885/a/../IB/", "http://host:0885/IB/", " http://host:885/IB/", "http://host:885/%49B/", "http://host:885//IB/", "ftp://host:885/IB/"] {
            assert!(endpoint(base, "odata/standard.odata/$metadata").is_none(), "{base}");
        }
    }
}
