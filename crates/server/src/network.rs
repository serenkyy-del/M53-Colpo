//! Common outbound TLS policy for integrations; no certificate validation bypass.
fn client_builder() -> Result<reqwest::ClientBuilder, Box<dyn std::error::Error + Send + Sync>> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30));
    if let Some(path) = std::env::var_os("M53_TLS_CA_FILE") {
        let bytes = std::fs::read(path)?;
        builder = builder.add_root_certificate(reqwest::Certificate::from_pem(&bytes)?);
    }
    Ok(builder)
}

pub fn client() -> Result<reqwest::Client, Box<dyn std::error::Error + Send + Sync>> {
    Ok(client_builder()?.build()?)
}

/// Direct callbacks must not route PDF contents or integration keys through
/// process/environment proxy settings. Other integrations retain their policy.
pub fn direct_client() -> Result<reqwest::Client, Box<dyn std::error::Error + Send + Sync>> {
    Ok(client_builder()?.no_proxy().build()?)
}
