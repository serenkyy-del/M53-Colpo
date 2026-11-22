//! Direct 1C authentication through its protected, read-only OData metadata.
//!
//! A successful credential response is accepted only after a separate anonymous
//! request to the same exact endpoint receives a 401 Basic challenge, and only
//! when the entire credential response is well-formed Edmx XML. No Core service,
//! browser session, publication redirect, or HTML response proves identity here.
//! Credentials, response bodies and upstream headers are never returned/logged.

use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};
use reqwest::{Client, StatusCode, Url, header};
use std::{collections::HashSet, fmt, time::Duration};

#[cfg(test)]
#[cfg(not(feature="standalone"))]
const HTTP_METADATA: &str = "http://192.168.73.45:885/UMZ_BIT-DEV/odata/standard.odata/$metadata";
#[cfg(feature="standalone")]
const HTTP_METADATA: &str = "";
const METADATA_SUFFIX: &str = "/odata/standard.odata/$metadata";
const EDMX_NAMESPACE: &[u8] = b"http://schemas.microsoft.com/ado/2007/06/edmx";
const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigurationError;

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("one_c_odata_auth_configuration")
    }
}
impl std::error::Error for ConfigurationError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    InvalidCredentials,
    Timeout,
    Transport,
    Redirect,
    AnonymousControl,
    UnexpectedStatus,
    BodyTooLarge,
    InvalidMetadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OdataAuthOutcome {
    Verified,
    ExplicitDenied,
    Unavailable(UnavailableReason),
}

/// Deliberately no Debug: the configured target must not leak through errors.
#[derive(Clone)]
pub struct OneCOdataAuth {
    endpoint: Url,
    client: Client,
    deadline: Duration,
}

impl OneCOdataAuth {
    pub fn from_env() -> Result<Option<Self>, ConfigurationError> {
        let endpoint = match std::env::var("M53_1C_ODATA_AUTH_URL") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(_) => return Err(ConfigurationError),
        };
        let allow_http = std::env::var("M53_1C_ODATA_ALLOW_HTTP").as_deref() == Ok("1");
        Self::new(&endpoint, allow_http).map(Some)
    }

    pub fn new(endpoint: &str, allow_http: bool) -> Result<Self, ConfigurationError> {
        Self::transport(validate_endpoint(endpoint, allow_http)?, REQUEST_TIMEOUT)
    }

    fn transport(endpoint: Url, deadline: Duration) -> Result<Self, ConfigurationError> {
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(deadline);
        if let Some(path) = std::env::var_os("M53_TLS_CA_FILE") {
            let bytes = std::fs::read(path).map_err(|_| ConfigurationError)?;
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(&bytes).map_err(|_| ConfigurationError)?,
            );
        }
        let client = builder.build().map_err(|_| ConfigurationError)?;
        Ok(Self {
            endpoint,
            client,
            deadline,
        })
    }

    /// The single five-second budget covers both GETs, body receipt and parsing.
    /// A caller still has to recheck the tenant/profile binding before issuing
    /// an M53 session. No identity or role is taken from the metadata contents.
    pub async fn check_credentials(&self, login: &str, password: &str) -> OdataAuthOutcome {
        if login.is_empty()
            || login.trim() != login
            || login.len() > 256
            || login.contains(':')
            || login.chars().any(char::is_control)
            || password.len() > 1024
            || password.contains('\0')
        {
            return OdataAuthOutcome::Unavailable(UnavailableReason::InvalidCredentials);
        }
        match tokio::time::timeout(self.deadline, self.check(login, password)).await {
            Ok(outcome) => outcome,
            Err(_) => OdataAuthOutcome::Unavailable(UnavailableReason::Timeout),
        }
    }

    async fn check(&self, login: &str, password: &str) -> OdataAuthOutcome {
        let anonymous = self
            .client
            .get(self.endpoint.clone())
            .header(header::ACCEPT, "application/xml")
            .send()
            .await;
        let anonymous = match anonymous {
            Ok(response) => response,
            Err(error) => return unavailable_transport(&error),
        };
        if anonymous.status().is_redirection() {
            return OdataAuthOutcome::Unavailable(UnavailableReason::Redirect);
        }
        if anonymous.status() != StatusCode::UNAUTHORIZED || !basic_challenge(anonymous.headers()) {
            return OdataAuthOutcome::Unavailable(UnavailableReason::AnonymousControl);
        }
        // Never reuse cookies or any identity of the anonymous request.
        drop(anonymous);
        let response = self
            .client
            .get(self.endpoint.clone())
            .header(header::ACCEPT, "application/xml")
            .basic_auth(login, Some(password))
            .send()
            .await;
        let mut response = match response {
            Ok(response) => response,
            Err(error) => return unavailable_transport(&error),
        };
        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            return OdataAuthOutcome::ExplicitDenied;
        }
        if response.status().is_redirection() {
            return OdataAuthOutcome::Unavailable(UnavailableReason::Redirect);
        }
        if response.status() != StatusCode::OK {
            return OdataAuthOutcome::Unavailable(UnavailableReason::UnexpectedStatus);
        }
        let xml_mime = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .is_some_and(|value| {
                value.eq_ignore_ascii_case("application/xml")
                    || value.eq_ignore_ascii_case("text/xml")
                    || value.eq_ignore_ascii_case("application/edmx+xml")
            });
        if !xml_mime {
            return OdataAuthOutcome::Unavailable(UnavailableReason::InvalidMetadata);
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return OdataAuthOutcome::Unavailable(UnavailableReason::BodyTooLarge);
        }
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(bytes)) => {
                    if body.len().saturating_add(bytes.len()) > MAX_RESPONSE_BYTES {
                        return OdataAuthOutcome::Unavailable(UnavailableReason::BodyTooLarge);
                    }
                    body.extend_from_slice(&bytes);
                }
                Ok(None) => break,
                Err(error) => return unavailable_transport(&error),
            }
        }
        // The metadata can be several MiB; parsing never occupies an async I/O
        // worker. The task consumes only the bounded document, not credentials.
        if tokio::task::spawn_blocking(move || valid_metadata(&body))
            .await
            .unwrap_or(false)
        {
            OdataAuthOutcome::Verified
        } else {
            OdataAuthOutcome::Unavailable(UnavailableReason::InvalidMetadata)
        }
    }
}

fn validate_endpoint(value: &str, allow_http: bool) -> Result<Url, ConfigurationError> {
    validate_endpoint_with_base(value, allow_http, std::env::var("M53_1C_BASE_URL").ok().as_deref())
}
fn validate_endpoint_with_base(value: &str, allow_http: bool, base: Option<&str>) -> Result<Url, ConfigurationError> {
    if value.trim() != value || value.contains('\\') {
        return Err(ConfigurationError);
    }
    let url = Url::parse(value).map_err(|_| ConfigurationError)?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().ends_with(METADATA_SUFFIX)
        || url
            .path()
            .trim_end_matches(METADATA_SUFFIX)
            .trim_matches('/')
            .is_empty()
        || !matches!(url.scheme(), "http" | "https")
        || (url.scheme() == "http" && (!allow_http || base.and_then(|base| crate::one_c_endpoint::endpoint(base, "odata/standard.odata/$metadata")).as_deref() != Some(value)))
    {
        return Err(ConfigurationError);
    }
    Ok(url)
}

fn unavailable_transport(error: &reqwest::Error) -> OdataAuthOutcome {
    OdataAuthOutcome::Unavailable(if error.is_timeout() {
        UnavailableReason::Timeout
    } else {
        UnavailableReason::Transport
    })
}

fn basic_challenge(headers: &header::HeaderMap) -> bool {
    headers
        .get_all(header::WWW_AUTHENTICATE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            let (mut quoted, mut escaped, mut start) = (false, false, 0);
            for (index, byte) in value.bytes().enumerate() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if byte == b'\\' && quoted {
                    escaped = true;
                    continue;
                }
                if byte == b'"' {
                    quoted = !quoted;
                }
                if byte == b',' && !quoted {
                    if value[start..index]
                        .split_ascii_whitespace()
                        .next()
                        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("Basic"))
                    {
                        return true;
                    }
                    start = index + 1;
                }
            }
            !quoted
                && value[start..]
                    .split_ascii_whitespace()
                    .next()
                    .is_some_and(|scheme| scheme.eq_ignore_ascii_case("Basic"))
        })
}

fn valid_xml_char(value: char) -> bool {
    matches!(value as u32, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

// XML 1.0 fifth-edition NameStartChar/NameChar, excluding ':' inside each
// namespace component. Namespace resolution alone does not validate QName
// characters in quick-xml's low-level reader.
fn name_start(value: char) -> bool {
    matches!(value as u32, 0x41..=0x5A | 0x5F | 0x61..=0x7A | 0xC0..=0xD6
        | 0xD8..=0xF6 | 0xF8..=0x2FF | 0x370..=0x37D | 0x37F..=0x1FFF
        | 0x200C..=0x200D | 0x2070..=0x218F | 0x2C00..=0x2FEF
        | 0x3001..=0xD7FF | 0xF900..=0xFDCF | 0xFDF0..=0xFFFD | 0x10000..=0xEFFFF)
}

fn valid_qname(value: &[u8]) -> bool {
    let Ok(value) = std::str::from_utf8(value) else {
        return false;
    };
    let components: Vec<_> = value.split(':').collect();
    (1..=2).contains(&components.len())
        && components.iter().all(|part| {
            let mut characters = part.chars();
            characters.next().is_some_and(name_start)
                && characters.all(|value| {
                    name_start(value)
                        || matches!(value as u32, 0x2D | 0x2E | 0x30..=0x39
                | 0xB7 | 0x300..=0x36F | 0x203F..=0x2040)
                })
        })
}

fn valid_metadata(body: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(body) else {
        return false;
    };
    if body.is_empty() || body.len() > MAX_RESPONSE_BYTES || !text.chars().all(valid_xml_char) {
        return false;
    }
    let mut reader = NsReader::from_str(text);
    reader.config_mut().check_end_names = true;
    reader.config_mut().check_comments = true;
    reader.config_mut().allow_unmatched_ends = false;
    reader.config_mut().expand_empty_elements = true;
    let (mut depth, mut root_seen, mut root_closed, mut declarations, mut data_services) =
        (0_usize, false, false, 0_usize, 0_usize);
    loop {
        let Ok(event) = reader.read_event() else {
            return false;
        };
        match event {
            Event::Start(element) => {
                if !valid_qname(element.name().as_ref()) {
                    return false;
                }
                let (namespace, local) = reader.resolver().resolve_element(element.name());
                if matches!(namespace, ResolveResult::Unknown(_)) || root_closed {
                    return false;
                }
                let is_edmx = matches!(&namespace,ResolveResult::Bound(namespace) if namespace.as_ref()==EDMX_NAMESPACE);
                if depth == 0 {
                    if root_seen || local.as_ref() != b"Edmx" || !is_edmx {
                        return false;
                    }
                    root_seen = true;
                } else if depth == 1 && local.as_ref() == b"DataServices" && is_edmx {
                    data_services += 1;
                    if data_services > 1 {
                        return false;
                    }
                }
                let mut attributes = HashSet::new();
                for attribute in element.attributes() {
                    let Ok(attribute) = attribute else {
                        return false;
                    };
                    if !valid_qname(attribute.key.as_ref()) {
                        return false;
                    }
                    let Ok(value) = attribute.normalized_value(quick_xml::XmlVersion::Explicit1_0)
                    else {
                        return false;
                    };
                    if !value.chars().all(valid_xml_char) {
                        return false;
                    }
                    if attribute.key.as_ref() == b"xmlns"
                        || attribute.key.as_ref().starts_with(b"xmlns:")
                    {
                        continue;
                    }
                    let (namespace, local) = reader.resolver().resolve_attribute(attribute.key);
                    let mut key = match namespace {
                        ResolveResult::Unknown(_) => return false,
                        ResolveResult::Unbound => Vec::new(),
                        ResolveResult::Bound(namespace) => namespace.as_ref().to_vec(),
                    };
                    key.push(0);
                    key.extend_from_slice(local.as_ref());
                    if !attributes.insert(key) {
                        return false;
                    }
                }
                // Empty elements are expanded by the reader, so every start
                // below has a corresponding End and one consistent depth path.
                depth += 1;
                if depth > 128 {
                    return false;
                }
            }
            Event::End(_) => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
                if depth == 0 {
                    root_closed = true;
                }
            }
            Event::Text(value) => {
                if depth == 0 && !value.as_ref().iter().all(u8::is_ascii_whitespace) {
                    return false;
                }
            }
            Event::CData(_) if depth == 0 => return false,
            Event::GeneralRef(value) => {
                if depth == 0 {
                    return false;
                }
                match value.resolve_char_ref() {
                    Ok(Some(value)) if valid_xml_char(value) => {}
                    Ok(None)
                        if matches!(value.as_ref(), b"amp" | b"lt" | b"gt" | b"apos" | b"quot") => {
                    }
                    _ => return false,
                }
            }
            Event::Decl(value) => {
                declarations += 1;
                if root_seen
                    || declarations > 1
                    || !text.trim_start_matches('\u{feff}').starts_with("<?xml")
                    || !value
                        .version()
                        .is_ok_and(|version| version.as_ref() == b"1.0")
                {
                    return false;
                }
                if value.encoding().is_some_and(|encoding| {
                    !encoding.is_ok_and(|encoding| encoding.eq_ignore_ascii_case(b"utf-8"))
                }) {
                    return false;
                }
            }
            Event::DocType(_) | Event::PI(_) | Event::Empty(_) => return false,
            Event::Eof => return root_seen && root_closed && depth == 0 && data_services == 1,
            Event::Comment(_) | Event::CData(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    fn validate_endpoint(value: &str, allow_http: bool) -> Result<reqwest::Url, super::ConfigurationError> {
        super::validate_endpoint_with_base(value, allow_http, Some("http://192.168.73.45:885/UMZ_BIT-DEV/"))
    }

    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };

    const METADATA: &[u8]=br#"<?xml version="1.0" encoding="UTF-8"?><edmx:Edmx xmlns:edmx="http://schemas.microsoft.com/ado/2007/06/edmx" Version="1.0"><edmx:DataServices><Schema xmlns="http://schemas.microsoft.com/ado/2008/09/edm" Namespace="Synthetic"><EntityType Name="Example"><Property Name="Value" Type="Edm.String" /></EntityType></Schema></edmx:DataServices></edmx:Edmx>"#;

    fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut value = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
            body.len()
        )
        .into_bytes();
        value.extend_from_slice(body);
        value
    }
    fn challenge() -> Vec<u8> {
        response(
            "401 Unauthorized",
            "WWW-Authenticate: Basic realm=\"fixture\", charset=\"UTF-8\"\r\n",
            b"",
        )
    }
    fn xml(body: &[u8]) -> Vec<u8> {
        response(
            "200 OK",
            "Content-Type: application/xml; charset=utf-8\r\n",
            body,
        )
    }

    async fn mock(
        responses: Vec<(Vec<u8>, Duration)>,
        deadline: Duration,
    ) -> (OneCOdataAuth, oneshot::Receiver<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let mut requests = Vec::new();
            for (response, delay) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 4096];
                loop {
                    let read = socket.read(&mut chunk).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8(request).unwrap());
                tokio::time::sleep(delay).await;
                let _ = socket.write_all(&response).await;
            }
            let _ = send.send(requests);
        });
        let endpoint = Url::parse(&format!(
            "http://{address}/fixture/odata/standard.odata/$metadata"
        ))
        .unwrap();
        (
            OneCOdataAuth::transport(endpoint, deadline).unwrap(),
            receive,
        )
    }

    #[test]
    fn configured_base_moves_auth_and_rejects_other_destinations() {
        for host in ["192.168.73.45", "192.168.73.46"] {
            let base = format!("http://{host}:885/UMZ_BIT-DEV/");
            let url = format!("{base}odata/standard.odata/$metadata");
            assert!(super::validate_endpoint_with_base(&url, true, Some(&base)).is_ok());
            assert!(super::validate_endpoint_with_base(&url, false, Some(&base)).is_err());
            assert!(super::validate_endpoint_with_base(&url, true, None).is_err());
            for other in [url.replace(host, "192.168.73.47"), url.replace(":885", ":886"), url.replace("UMZ_BIT-DEV", "OTHER")] {
                assert!(super::validate_endpoint_with_base(&other, true, Some(&base)).is_err());
            }
        }
    }

    #[test]
    fn moved_test_metadata_host_is_exactly_allowed() {
        assert!(validate_endpoint("http://192.168.73.45:885/UMZ_BIT-DEV/odata/standard.odata/$metadata", true).is_ok());
        assert!(validate_endpoint("http://192.168.73.18:885/UMZ_BIT-DEV/odata/standard.odata/$metadata", true).is_err());
    }

    #[test]
    #[cfg(not(feature="standalone"))]
    fn only_explicit_correct_development_publication_accepts_http() {
        assert!(validate_endpoint(HTTP_METADATA, true).is_ok());
        assert!(validate_endpoint(HTTP_METADATA, false).is_err());
        assert!(
            validate_endpoint(
                "https://example.test/IB/odata/standard.odata/$metadata",
                false
            )
            .is_ok()
        );
        for endpoint in [
            "http://192.168.73.45:885/UMZ_BIT/odata/standard.odata/$metadata",
            "http://192.168.73.45:885/UMZ_BIT_DEV/odata/standard.odata/$metadata",
            "http://192.168.73.45:885/UMZ_BIR-DEV/odata/standard.odata/$metadata",
            "http://127.0.0.1:8930/odata/$metadata",
            "https://user:password@example.test/IB/odata/standard.odata/$metadata",
            "https://example.test/IB/odata/standard.odata/$metadata?password=secret",
            "https://example.test/IB/odata/standard.odata/$metadata#secret",
            "https://example.test/odata/standard.odata/$metadata",
            "https://example.test/IB/e1cib/start",
        ] {
            assert!(validate_endpoint(endpoint, true).is_err());
        }
    }

    #[test]
    fn basic_challenge_is_a_scheme_not_a_word_inside_a_quoted_realm() {
        for (value, expected) in [
            ("bAsIc realm=\"fixture\"", true),
            (
                "Negotiate, Basic realm=\"fixture\", charset=\"UTF-8\"",
                true,
            ),
            ("Digest realm=\"not, Basic credentials\"", false),
            ("Bearer realm=\"Basic\"", false),
            ("Basic realm=\"unterminated", false),
        ] {
            let mut headers = header::HeaderMap::new();
            headers.insert(header::WWW_AUTHENTICATE, value.parse().unwrap());
            assert_eq!(basic_challenge(&headers), expected);
        }
    }

    #[test]
    fn fully_parses_namespaces_structure_entities_and_document_end() {
        assert!(valid_metadata(METADATA));
        for invalid in [
            "<html><body>Login</body></html>",
            "<Edmx><DataServices/></Edmx>",
            "<e:Edmx xmlns:e='wrong'><e:DataServices/></e:Edmx>",
            "<e:Edmx xmlns:e='http://schemas.microsoft.com/ado/2007/06/edmx'><DataServices/></e:Edmx>",
            "<e:Edmx xmlns:e='http://schemas.microsoft.com/ado/2007/06/edmx'><e:DataServices/><e:DataServices/></e:Edmx>",
            "<e:Edmx xmlns:e='http://schemas.microsoft.com/ado/2007/06/edmx'><e:DataServices></e:Edmx>",
            "<!DOCTYPE x [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><x/>",
        ] {
            assert!(!valid_metadata(invalid.as_bytes()));
        }
        for suffix in [
            b"<extra/>".as_slice(),
            b"trailing".as_slice(),
            b"\0".as_slice(),
        ] {
            let mut invalid = METADATA.to_vec();
            invalid.extend_from_slice(suffix);
            assert!(!valid_metadata(&invalid));
        }
        let mut truncated = METADATA.to_vec();
        truncated.truncate(truncated.len() - 5);
        assert!(!valid_metadata(&truncated));
        let document = std::str::from_utf8(METADATA).unwrap();
        for replacement in [
            "<Property Name=\"Value\" Name=\"Duplicate\" Type=\"Edm.String\" />",
            "<0Property Name=\"Value\" />",
            "<Property Name=\"&undefined;\" />",
            "<Property Name=\"&#0;\" />",
            "<Property missing:Name=\"Value\" />",
        ] {
            assert!(!valid_metadata(
                document
                    .replace(
                        "<Property Name=\"Value\" Type=\"Edm.String\" />",
                        replacement
                    )
                    .as_bytes()
            ));
        }
        assert!(!valid_metadata(format!(" \n{document}").as_bytes()));
    }

    #[tokio::test]
    async fn protected_metadata_verifies_without_bodies_cookies_or_query_credentials() {
        let (client, requests) = mock(
            vec![
                (challenge(), Duration::ZERO),
                (xml(METADATA), Duration::ZERO),
            ],
            REQUEST_TIMEOUT,
        )
        .await;
        assert_eq!(
            client
                .check_credentials("Тестовый Врач", "synthetic:+&pass")
                .await,
            OdataAuthOutcome::Verified
        );
        let requests = requests.await.unwrap();
        assert_eq!(requests.len(), 2);
        for request in &requests {
            assert!(
                request.starts_with("GET /fixture/odata/standard.odata/$metadata HTTP/1.1\r\n")
            );
            assert!(!request.to_ascii_lowercase().contains("cookie:"));
            assert!(request.split_once("\r\n\r\n").unwrap().1.is_empty());
        }
        assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
        let authorization = requests[1]
            .lines()
            .find_map(|line| line.strip_prefix("authorization: Basic "))
            .unwrap();
        assert_eq!(
            STANDARD.decode(authorization).unwrap(),
            "Тестовый Врач:synthetic:+&pass".as_bytes()
        );
    }

    #[tokio::test]
    async fn anonymous_access_and_non_basic_challenges_never_send_credentials() {
        for first in [
            xml(METADATA),
            response("401 Unauthorized", "", b""),
            response(
                "401 Unauthorized",
                "WWW-Authenticate: Digest realm=\"x, Basic fake\"\r\n",
                b"",
            ),
            response("403 Forbidden", "", b""),
        ] {
            let (client, requests) = mock(vec![(first, Duration::ZERO)], REQUEST_TIMEOUT).await;
            assert_eq!(
                client.check_credentials("Test", "synthetic-only").await,
                OdataAuthOutcome::Unavailable(UnavailableReason::AnonymousControl)
            );
            let requests = requests.await.unwrap();
            assert_eq!(requests.len(), 1);
            assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
        }
    }

    #[tokio::test]
    async fn invalid_credentials_and_anonymous_redirect_never_send_a_password() {
        let (client, mut requests) =
            mock(vec![(challenge(), Duration::ZERO)], REQUEST_TIMEOUT).await;
        for (login, password) in [
            ("", "synthetic-only"),
            ("Test ", "synthetic-only"),
            ("Test:realm", "synthetic-only"),
            ("Test", "null\0byte"),
        ] {
            assert_eq!(
                client.check_credentials(login, password).await,
                OdataAuthOutcome::Unavailable(UnavailableReason::InvalidCredentials)
            );
        }
        assert!(matches!(
            requests.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        let (client, requests) = mock(
            vec![(
                response(
                    "302 Found",
                    "Location: http://127.0.0.1:1/forbidden\r\n",
                    b"",
                ),
                Duration::ZERO,
            )],
            REQUEST_TIMEOUT,
        )
        .await;
        assert_eq!(
            client.check_credentials("Test", "synthetic-only").await,
            OdataAuthOutcome::Unavailable(UnavailableReason::Redirect)
        );
        let requests = requests.await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
    }

    #[tokio::test]
    async fn credential_denials_redirects_and_false_successes_are_distinct() {
        for (second, expected) in [
            (
                response("401 Unauthorized", "", b""),
                OdataAuthOutcome::ExplicitDenied,
            ),
            (
                response("403 Forbidden", "", b""),
                OdataAuthOutcome::ExplicitDenied,
            ),
            (
                response(
                    "302 Found",
                    "Location: http://127.0.0.1:1/forbidden\r\n",
                    b"",
                ),
                OdataAuthOutcome::Unavailable(UnavailableReason::Redirect),
            ),
            (
                response("500 Server Error", "", b""),
                OdataAuthOutcome::Unavailable(UnavailableReason::UnexpectedStatus),
            ),
            (
                response("200 OK", "Content-Type: text/html\r\n", METADATA),
                OdataAuthOutcome::Unavailable(UnavailableReason::InvalidMetadata),
            ),
            (
                xml(b"<html>Login</html>"),
                OdataAuthOutcome::Unavailable(UnavailableReason::InvalidMetadata),
            ),
        ] {
            let (client, _) = mock(
                vec![(challenge(), Duration::ZERO), (second, Duration::ZERO)],
                REQUEST_TIMEOUT,
            )
            .await;
            assert_eq!(
                client.check_credentials("Test", "synthetic-only").await,
                expected
            );
        }
    }

    #[tokio::test]
    async fn bodies_and_whole_two_request_duration_are_bounded() {
        let second=format!("HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",MAX_RESPONSE_BYTES+1).into_bytes();
        let (client, _) = mock(
            vec![(challenge(), Duration::ZERO), (second, Duration::ZERO)],
            REQUEST_TIMEOUT,
        )
        .await;
        assert_eq!(
            client.check_credentials("Test", "synthetic-only").await,
            OdataAuthOutcome::Unavailable(UnavailableReason::BodyTooLarge)
        );
        let mut second =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nConnection: close\r\n\r\n"
                .to_vec();
        second.extend(std::iter::repeat_n(b'a', MAX_RESPONSE_BYTES + 1));
        let (client, _) = mock(
            vec![(challenge(), Duration::ZERO), (second, Duration::ZERO)],
            REQUEST_TIMEOUT,
        )
        .await;
        assert_eq!(
            client.check_credentials("Test", "synthetic-only").await,
            OdataAuthOutcome::Unavailable(UnavailableReason::BodyTooLarge)
        );
        let (client, _) = mock(
            vec![
                (challenge(), Duration::from_millis(60)),
                (xml(METADATA), Duration::from_millis(60)),
            ],
            Duration::from_millis(100),
        )
        .await;
        assert_eq!(
            client.check_credentials("Test", "synthetic-only").await,
            OdataAuthOutcome::Unavailable(UnavailableReason::Timeout)
        );
    }

    #[tokio::test]
    async fn multi_megabyte_valid_metadata_completes_within_the_auth_budget() {
        let mut body=b"<?xml version=\"1.0\"?><e:Edmx xmlns:e=\"http://schemas.microsoft.com/ado/2007/06/edmx\"><e:DataServices><!--".to_vec();
        body.extend(std::iter::repeat_n(b'a', 4 * 1024 * 1024));
        body.extend_from_slice(b"--></e:DataServices></e:Edmx>");
        let (client, _) = mock(
            vec![(challenge(), Duration::ZERO), (xml(&body), Duration::ZERO)],
            REQUEST_TIMEOUT,
        )
        .await;
        assert_eq!(
            client.check_credentials("Test", "synthetic-only").await,
            OdataAuthOutcome::Verified
        );
    }
}
