//! The operator configures the origin. TLS uses the operating system trust store.
pub fn initialize() -> Result<(), String> {
    Ok(())
}

pub fn cache_origin(origin: &str) -> String {
    normalize(origin).unwrap_or_else(|_| origin.trim_end_matches('/').to_owned())
}

pub fn same_server(a: &str, b: &str) -> bool {
    cache_origin(a) == cache_origin(b)
}

pub fn effective_origin(origin: &str, key: &str) -> Result<String, String> {
    saved_origin(origin, key)
}

pub fn saved_origin(origin: &str, _key: &str) -> Result<String, String> {
    normalize(origin)
}

fn normalize(origin: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(origin).map_err(|_| "Некорректный адрес сервера")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || url.path() != "/"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Укажите корневой адрес сервера без пути и учётных данных".into());
    }
    Ok(url.origin().ascii_serialization())
}

pub fn blocking(origin: &str) -> Result<reqwest::blocking::ClientBuilder, String> {
    normalize(origin)?;
    Ok(reqwest::blocking::Client::builder().no_proxy().tls_built_in_webpki_certs(false))
}

pub fn asynchronous(origin: &str) -> Result<reqwest::ClientBuilder, String> {
    normalize(origin)?;
    Ok(reqwest::Client::builder().no_proxy().tls_built_in_webpki_certs(false))
}
