//! Bounded direct transport for the standard 1C web-client login form.
//!
//! The documented endpoint is POST `e1cib/start`, form fields `usr`, `pwd`, and
//! `authfailhandling=error`. The platform documents 402 for failed authentication
//! and creates a web-client session on success, but does not document a stable
//! machine-readable success proof. Consequently this preparation module NEVER
//! authenticates an M53 user: every 2xx is `Unverified`, including HTML, cookies,
//! JSON, and empty responses. No caller can obtain a verified-user outcome.
//!
//! Upstream credentials are submitted only to the administrator-configured 1C
//! publication, not to OData Core. No credentials, URLs, cookies, or response
//! bodies are included in errors. No production/test publication is contacted
//! by this module's tests; they use private local mock listeners.

use reqwest::{Client, StatusCode, Url};
use std::{fmt, time::Duration};

#[cfg(not(feature="standalone"))]
const TEST_BASE: &str = "http://192.168.73.18:885/UMZ_BIT_DEV/";
#[cfg(feature="standalone")]
const TEST_BASE: &str = "";
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfigurationError;

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("one_c_web_auth_configuration")
    }
}

impl std::error::Error for ConfigurationError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    InvalidCredentials,
    Timeout,
    Transport,
    Redirect,
    BodyTooLarge,
    UnexpectedStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebAuthOutcome {
    ExplicitDenied,
    /// Transport succeeded; the response does not prove authenticated identity.
    Unverified,
    Unavailable(UnavailableReason),
}

#[derive(Clone)]
pub struct OneCWebAuth {
    endpoint: Url,
    client: Client,
}

impl OneCWebAuth {
    /// An unset base disables the adapter. No production endpoint is assumed.
    pub fn from_env() -> Result<Option<Self>, ConfigurationError> {
        let base = match std::env::var("M53_1C_WEB_BASE_URL") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => return Ok(None),
            Err(_) => return Err(ConfigurationError),
        };
        let allow_test_http = std::env::var("M53_1C_WEB_ALLOW_TEST_HTTP").as_deref() == Ok("1");
        Self::new(&base, allow_test_http).map(Some)
    }

    /// HTTPS uses normal certificate validation. The only plain HTTP exception
    /// is the exact explicitly authorized development publication and flag.
    pub fn new(publication_base: &str, allow_test_http: bool) -> Result<Self, ConfigurationError> {
        let endpoint = endpoint(publication_base, allow_test_http)?;
        Self::transport(endpoint, REQUEST_TIMEOUT)
    }

    fn transport(endpoint: Url, timeout: Duration) -> Result<Self, ConfigurationError> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(timeout)
            .build()
            .map_err(|_| ConfigurationError)?;
        Ok(Self { endpoint, client })
    }

    /// This function classifies a web response. It does not return an M53
    /// session, an offline grant, an authenticated identity, or upstream cookies.
    pub async fn check_credentials(&self, login: &str, password: &str) -> WebAuthOutcome {
        if login.is_empty()
            || login.trim() != login
            || login.len() > 256
            || login.chars().any(char::is_control)
            || password.len() > 1024
            || password.contains('\0')
        {
            return WebAuthOutcome::Unavailable(UnavailableReason::InvalidCredentials);
        }
        let response = self
            .client
            .post(self.endpoint.clone())
            .form(&[
                ("usr", login),
                ("pwd", password),
                ("authfailhandling", "error"),
            ])
            .send()
            .await;
        let mut response = match response {
            Ok(response) => response,
            Err(error) => return WebAuthOutcome::Unavailable(transport_failure(&error)),
        };
        let status = response.status();
        if matches!(
            status,
            StatusCode::UNAUTHORIZED | StatusCode::PAYMENT_REQUIRED | StatusCode::FORBIDDEN
        ) {
            return WebAuthOutcome::ExplicitDenied;
        }
        if status.is_redirection() {
            return WebAuthOutcome::Unavailable(UnavailableReason::Redirect);
        }
        if !status.is_success() {
            return WebAuthOutcome::Unavailable(UnavailableReason::UnexpectedStatus);
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return WebAuthOutcome::Unavailable(UnavailableReason::BodyTooLarge);
        }
        let mut size = 0_usize;
        loop {
            match response.chunk().await {
                Ok(Some(bytes)) => {
                    size = size.saturating_add(bytes.len());
                    if size > MAX_RESPONSE_BYTES {
                        return WebAuthOutcome::Unavailable(UnavailableReason::BodyTooLarge);
                    }
                }
                Ok(None) => return WebAuthOutcome::Unverified,
                Err(error) => return WebAuthOutcome::Unavailable(transport_failure(&error)),
            }
        }
    }
}

fn endpoint(base: &str, allow_test_http: bool) -> Result<Url, ConfigurationError> {
    if base.is_empty() || base.trim() != base || base.contains('\\') {
        return Err(ConfigurationError);
    }
    let mut url = Url::parse(base).map_err(|_| ConfigurationError)?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.scheme(), "http" | "https")
        || (url.scheme() == "http" && (!allow_test_http || base != TEST_BASE))
        || url.path().trim_matches('/').is_empty()
        || !url.path().ends_with('/')
    {
        return Err(ConfigurationError);
    }
    url.path_segments_mut()
        .map_err(|_| ConfigurationError)?
        .pop_if_empty()
        .push("e1cib")
        .push("start");
    Ok(url)
}

fn transport_failure(error: &reqwest::Error) -> UnavailableReason {
    if error.is_timeout() {
        UnavailableReason::Timeout
    } else {
        UnavailableReason::Transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
    };

    async fn mock(response: Vec<u8>, delay: Duration) -> (OneCWebAuth, oneshot::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if let Some(boundary) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers =
                        String::from_utf8_lossy(&request[..boundary]).to_ascii_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap_or("0")
                        .parse()
                        .unwrap();
                    if request.len() >= boundary + 4 + length {
                        break;
                    }
                }
            }
            let _ = send.send(request);
            tokio::time::sleep(delay).await;
            let _ = socket.write_all(&response).await;
        });
        let endpoint = Url::parse(&format!("http://{address}/test/e1cib/start")).unwrap();
        // This constructor is private. Runtime URL validation never allows
        // arbitrary loopback HTTP as a substitute for the authorized target.
        (
            OneCWebAuth::transport(endpoint, REQUEST_TIMEOUT).unwrap(),
            receive,
        )
    }

    fn response(status: &str, extra_headers: &str, body: &[u8]) -> Vec<u8> {
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    #[test]
    #[cfg(not(feature="standalone"))]
    fn endpoint_is_explicit_and_plain_http_is_exactly_scoped() {
        for (base, flag) in [
            (TEST_BASE, false),
            ("http://192.168.73.18:885/UMZ_BIT/", true),
            ("http://192.168.73.18:885/UMZ_BIT_DEV/?token=secret", true),
            ("http://127.0.0.1:8930/odata/", true),
            ("https://username:password@example.test/IB/", false),
            ("https://example.test/IB/?password=secret", false),
            ("https://example.test/IB/#secret", false),
            ("https://example.test/IB/\\other", false),
            ("https://example.test/IB", false),
            ("https://example.test/", false),
            ("ftp://example.test/IB/", false),
            (" https://example.test/IB/", false),
        ] {
            assert!(matches!(endpoint(base, flag), Err(ConfigurationError)));
        }
        assert_eq!(
            endpoint(TEST_BASE, true).unwrap().as_str(),
            "http://192.168.73.18:885/UMZ_BIT_DEV/e1cib/start"
        );
        assert_eq!(
            endpoint("https://example.test/IB/", false)
                .unwrap()
                .as_str(),
            "https://example.test/IB/e1cib/start"
        );
        assert_eq!(
            ConfigurationError.to_string(),
            "one_c_web_auth_configuration"
        );
    }

    #[tokio::test]
    async fn credentials_are_posted_only_as_form_body_and_are_encoded() {
        let (client, request) = mock(
            response("402 Payment Required", "", b"private upstream error"),
            Duration::ZERO,
        )
        .await;
        let login = "Тестовый Врач";
        let password = "a&b=+%\" secret";
        assert_eq!(
            client.check_credentials(login, password).await,
            WebAuthOutcome::ExplicitDenied
        );
        let request = String::from_utf8(request.await.unwrap()).unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /test/e1cib/start HTTP/1.1\r\n"));
        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
        assert!(!headers.to_ascii_lowercase().contains("cookie:"));
        assert!(!headers.contains(login) && !headers.contains(password));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("content-type: application/x-www-form-urlencoded")
        );
        let parsed = Url::parse(&format!("https://example.test/?{body}")).unwrap();
        let fields: HashMap<_, _> = parsed.query_pairs().collect();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields["usr"], login);
        assert_eq!(fields["pwd"], password);
        assert_eq!(fields["authfailhandling"], "error");
    }

    #[tokio::test]
    async fn every_success_response_remains_unverified() {
        for (status, headers, body) in [
            (
                "200 OK",
                "Content-Type: text/html\r\n",
                b"<html>login form or application</html>".as_slice(),
            ),
            (
                "200 OK",
                "Content-Type: application/json\r\n",
                b"{\"authenticated\":true}".as_slice(),
            ),
            (
                "200 OK",
                "Set-Cookie: web-session=arbitrary; HttpOnly\r\n",
                b"".as_slice(),
            ),
            ("204 No Content", "", b"".as_slice()),
        ] {
            let (client, _) = mock(response(status, headers, body), Duration::ZERO).await;
            assert_eq!(
                client.check_credentials("Test user", "").await,
                WebAuthOutcome::Unverified
            );
        }
    }

    #[tokio::test]
    async fn explicit_denials_and_other_statuses_are_distinct() {
        for status in ["401 Unauthorized", "402 Payment Required", "403 Forbidden"] {
            let (client, _) = mock(
                response(status, "", b"upstream-private-body"),
                Duration::ZERO,
            )
            .await;
            assert_eq!(
                client.check_credentials("Test", "synthetic-only").await,
                WebAuthOutcome::ExplicitDenied
            );
        }
        for status in [
            "400 Bad Request",
            "404 Not Found",
            "429 Too Many Requests",
            "500 Server Error",
        ] {
            let (client, _) = mock(
                response(status, "", b"upstream-private-body"),
                Duration::ZERO,
            )
            .await;
            assert_eq!(
                client.check_credentials("Test", "synthetic-only").await,
                WebAuthOutcome::Unavailable(UnavailableReason::UnexpectedStatus)
            );
        }
        let (client, _) = mock(
            response(
                "302 Found",
                "Location: http://127.0.0.1:1/must-not-follow\r\n",
                b"",
            ),
            Duration::ZERO,
        )
        .await;
        assert_eq!(
            client.check_credentials("Test", "synthetic-only").await,
            WebAuthOutcome::Unavailable(UnavailableReason::Redirect)
        );
    }

    #[tokio::test]
    async fn oversized_and_stalled_bodies_are_bounded_without_success() {
        for data in [
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_RESPONSE_BYTES + 1
            )
            .into_bytes(),
            {
                let body = vec![b'a'; MAX_RESPONSE_BYTES + 1];
                let mut data = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
                data.extend_from_slice(&body);
                data
            },
        ] {
            let (client, _) = mock(data, Duration::ZERO).await;
            assert_eq!(
                client.check_credentials("Test", "synthetic-only").await,
                WebAuthOutcome::Unavailable(UnavailableReason::BodyTooLarge)
            );
        }
        let (mut client, _) = mock(response("200 OK", "", b""), Duration::from_millis(100)).await;
        client.client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(20))
            .build()
            .unwrap();
        assert_eq!(
            client.check_credentials("Test", "synthetic-only").await,
            WebAuthOutcome::Unavailable(UnavailableReason::Timeout)
        );
    }

    #[tokio::test]
    async fn invalid_login_never_reaches_upstream_and_errors_reveal_no_input() {
        let (client, mut request) = mock(response("200 OK", "", b""), Duration::ZERO).await;
        for login in ["", "   ", "not-trimmed ", "line\nbreak"] {
            assert_eq!(
                client.check_credentials(login, "synthetic-only").await,
                WebAuthOutcome::Unavailable(UnavailableReason::InvalidCredentials)
            );
        }
        assert_eq!(
            client.check_credentials("Test", &"a".repeat(1025)).await,
            WebAuthOutcome::Unavailable(UnavailableReason::InvalidCredentials)
        );
        assert!(matches!(
            request.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(
            !format!(
                "{:?}",
                WebAuthOutcome::Unavailable(UnavailableReason::Transport)
            )
            .contains("synthetic-only")
        );
    }
}
