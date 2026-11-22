use m53_contracts::workflow::*;
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::time::Duration;
use uuid::Uuid;
pub type Result<T> = std::result::Result<T, String>;
#[derive(Clone)]
pub struct Connection {
    pub offline: Option<std::sync::Arc<crate::offline::Offline>>,
    pub user_id: Option<Uuid>,
    pub doctor: String,
    pub base: String,
    /// Stable local namespace; transport upgrades never move clinical assets.
    pub storage_base: String,
    pub key: String,
    pub session: String,
    pub offline_permissions: Option<m53_contracts::access::OfflinePermissions>,
    pub(crate) offline_deadline: Option<(i64,std::time::Instant)>,
    pub(crate) identity: Option<crate::ui_guidance::Identity>,
    client: reqwest::blocking::Client,
}
impl Connection {
    pub fn new(base: &str, key: &str) -> Result<Self> {
        crate::transport::initialize()?;
        let storage_base = crate::transport::cache_origin(base);
        let base = crate::transport::effective_origin(base, key)?;
        let url = reqwest::Url::parse(&base).map_err(|_| "Некорректный адрес сервиса")?;
        if !allowed_transport(&url)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Требуется защищённое подключение HTTPS".into());
        }
        let client = crate::transport::blocking(&base)?
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(8))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(8)
            .tcp_nodelay(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "Ошибка HTTP клиента")?;
        Ok(Self {
            offline: None,
            user_id: None,
            doctor: String::new(),
            base: base.trim_end_matches('/').into(),
            storage_base,
            key: key.into(),
            session: String::new(),
            offline_permissions: None,
            offline_deadline: None,
            identity: None,
            client,
        })
    }
    pub(crate) fn event_client(&self) -> Result<reqwest::Client> {
        let builder = crate::transport::asynchronous(&self.base)?
            .connect_timeout(Duration::from_secs(2))
            .read_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none());
        builder.build().map_err(|_| "Ошибка подписки HTTP".into())
    }
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        self.client
            .request(method, format!("{}{}", self.base, path))
            .header("X-Workstation-Key", &self.key)
            .header("X-M53-Protocol-Version", "2")
            .header("X-M53-Report-Format", "letterhead-v1")
            .bearer_auth(&self.session)
    }
    fn decode<T: DeserializeOwned>(
        r: std::result::Result<reqwest::blocking::Response, reqwest::Error>,
    ) -> Result<T> {
        let r = r.map_err(|_| "Сервис недоступен: проверьте сеть и TLS")?;
        if !r.status().is_success() {
            let status = r.status();
            return Err(format!(
                "Ошибка сервиса {status}. {}",
                r.json::<serde_json::Value>()
                    .ok()
                    .and_then(|v| v.get("code").and_then(|s| s.as_str()).map(str::to_owned))
                    .unwrap_or_default()
            ));
        }
        r.json().map_err(|_| "Некорректный ответ сервиса".into())
    }
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        if let Some(offline) = &self.offline {
            return serde_json::from_value(offline.get(path)?)
                .map_err(|_| "Кеш: структура данных еще не готова".into());
        }
        Self::decode(self.request(reqwest::Method::GET, path).send())
    }
    pub fn post<I: Serialize, T: DeserializeOwned>(&self, path: &str, input: &I) -> Result<T> {
        if let Some(offline) = &self.offline {
            return serde_json::from_value(offline.mutate(
                path,
                "POST",
                serde_json::to_value(input).map_err(|_| "Serialization failed")?,
            )?)
            .map_err(|_| "Неверный локальный ответ".into());
        }
        let request=self.request(reqwest::Method::POST,path);
        let request=if path.ends_with("/manual-identity-verification") {request.header("X-Operation-ID",Uuid::new_v4().to_string())}else{request};
        Self::decode(request.json(input).send())
    }
    pub fn put<I: Serialize, T: DeserializeOwned>(&self, path: &str, input: &I) -> Result<T> {
        if let Some(offline) = &self.offline {
            return serde_json::from_value(offline.mutate(
                path,
                "PUT",
                serde_json::to_value(input).map_err(|_| "Serialization failed")?,
            )?)
            .map_err(|_| "Неверный локальный ответ".into());
        }
        Self::decode(self.request(reqwest::Method::PUT, path).json(input).send())
    }
    pub fn login(&mut self, login: String, password: String) -> Result<String> {
        let session: Session = self.post("/api/v1/login", &Login { login, password })?;
        self.session = session.token;
        self.user_id = Some(session.user_id);
        self.doctor = session.display_name.clone();
        self.identity = None;
        Ok(session.display_name)
    }
    pub fn health_diagnostic(&self) -> Result<()> {
        self.client
            .get(format!("{}/health", self.base))
            .send()
            .map_err(|e| format!("Проверка публичного health: {e:?}"))?
            .error_for_status()
            .map_err(|_| "Health status error")?;
        Ok(())
    }
    pub fn auth_profiles(&self) -> Result<Vec<crate::one_c_auth::Profile>> {
        let mut items = vec![];
        let mut after = None;
        let mut seen = std::collections::BTreeSet::new();
        let mut organization = None;
        loop {
            let path = after
                .map(|id| format!("/api/v1/auth/profiles?limit=100&after={id}"))
                .unwrap_or_else(|| "/api/v1/auth/profiles?limit=100".into());
            let page: crate::one_c_auth::Page = self.get(&path)?;
            if page.auth_provider != "1c_web"
                || page.organization_id.is_nil()
                || page.server_time <= 0
                || organization.is_some_and(|org| org != page.organization_id)
            {
                return Err("Некорректный каталог врачей".into());
            }
            organization = Some(page.organization_id);
            for item in page.items {
                if item.doctor_id.is_nil()
                    || item.binding_version <= 0
                    || item.user_guid.is_nil()
                    || !seen.insert(item.doctor_id)
                {
                    return Err("Некорректная привязка врача".into());
                }
                items.push(item);
            }
            match page.next_after {
                Some(next) if Some(next) != after && items.len() < 10000 => after = Some(next),
                None => return Ok(items),
                _ => return Err("Некорректная пагинация врачей".into()),
            }
        }
    }
    pub fn doctor_directory(&self) -> Result<Vec<m53_contracts::access::DoctorIdentity>> {
        let mut result = vec![];
        let mut after = None;
        let mut seen = std::collections::BTreeSet::new();
        loop {
            let path = after
                .map(|id| format!("/api/v1/doctors?limit=100&after={id}"))
                .unwrap_or_else(|| "/api/v1/doctors?limit=100".into());
            let page: m53_contracts::catalog::DoctorPage = self.get(&path)?;
            for doctor in page.items {
                if !seen.insert(doctor.id) {
                    return Err("Некорректная страница справочника врачей".into());
                }
                result.push(doctor);
            }
            match page.next_after {
                None => return Ok(result),
                Some(next) if Some(next) != after && result.len() < 100000 => after = Some(next),
                _ => return Err("Некорректный курсор справочника врачей".into()),
            }
        }
    }
    pub(crate) fn require_offline_access(&self) -> Result<()> {
        if self.offline_deadline.is_some_and(|(expires,deadline)|
            crate::one_c_auth::now()>=expires || std::time::Instant::now()>=deadline) {
            return Err("Автономный допуск на 30 дней истёк; требуется онлайн-вход. Введённое сохранено локально".into());
        }
        Ok(())
    }
    pub(crate) fn apply_offline_claims(&mut self, claims:m53_contracts::access::OfflineClaims) -> Result<()> {
        let remaining=claims.exp.checked_sub(crate::one_c_auth::now()).filter(|s|*s>0)
            .ok_or("Автономный допуск истёк; требуется онлайн-вход")?;
        let deadline=std::time::Instant::now().checked_add(Duration::from_secs(remaining as u64))
            .ok_or("Некорректный срок автономного допуска")?;
        self.offline_deadline=Some((claims.exp,deadline));
        self.identity=Some(crate::ui_guidance::Identity {
            organization_id: claims.organization_id,
            user_id: claims.sub,
            doctor_id: claims.doctor_id,
            workstation_id: claims.workstation_id,
        });
        self.offline_permissions=Some(claims.permissions);
        Ok(())
    }
    pub fn login_1c(
        &mut self,
        selected: crate::one_c_auth::Profile,
        password: &str,
    ) -> Result<crate::one_c_auth::Prepared> {
        // User-authorized test HTTP uses the same transport gate as Connection::new.
        let key: m53_contracts::access::OfflinePublicKey = self.get("/api/v1/auth/offline-key")?;
        let input = m53_contracts::one_c_access::OneCLoginRequest {
            doctor_id: selected.doctor_id,
            binding_version: selected.binding_version,
            password: password.into(),
        };
        if !input.valid() {
            return Err("Некорректный запрос входа 1С".into());
        }
        let bundle = self.post("/api/v1/auth/1c/login", &input)?;
        let (prepared, session) = crate::one_c_auth::Prepared::from_bundle(selected, bundle, key)?;
        self.apply_offline_claims(crate::one_c_auth::verify(&prepared.grant, &prepared.pinned_key)?)?;
        self.session = session.token;
        self.user_id = Some(session.user_id);
        self.doctor = session.display_name;
        Ok(prepared)
    }
    pub fn download(&self, id: Uuid) -> Result<Vec<u8>> {
        if let Some(offline) = &self.offline {
            return offline.download(id);
        }
        let r = self
            .request(reqwest::Method::GET, &format!("/api/v1/assets/{id}"))
            .timeout(Duration::from_secs(60))
            .send()
            .map_err(|_| "Ошибка загрузки файла")?;
        if !r.status().is_success() {
            return Err("Файл недоступен".into());
        }
        Ok(r.bytes().map_err(|_| "Неполный файл")?.to_vec())
    }
    pub fn download_shared(&self, id: Uuid) -> Result<std::sync::Arc<Vec<u8>>> {
        if let Some(offline) = &self.offline {
            offline.download_shared(id)
        } else {
            self.download(id).map(std::sync::Arc::new)
        }
    }
    pub fn cached_original_shared(&self, id: Uuid) -> Result<Option<std::sync::Arc<Vec<u8>>>> {
        self.offline.as_ref().map(|offline| offline.cached_original_shared(id)).unwrap_or(Ok(None))
    }
    pub fn prepare_asset(&self, id: Uuid) -> Result<Option<std::sync::Arc<Vec<u8>>>> {
        if let Some(offline) = &self.offline { offline.prepare_asset(id) }
        else { self.download_shared(id).map(Some) }
    }
    pub fn asset_failure(&self, id: Uuid) -> Option<crate::offline::AssetFailure> {
        self.offline.as_ref().and_then(|offline| offline.asset_failure(id))
    }
    pub(crate) fn download_timed(&self, id: Uuid, budget: Duration) -> Result<std::sync::Arc<Vec<u8>>> {
        let response = self.request(reqwest::Method::GET, &format!("/api/v1/assets/{id}"))
            .timeout(budget).send().map_err(|_| "Не удалось загрузить выбранный снимок в срок")?;
        if !response.status().is_success() {
            return Err(crate::offline::AssetFailure::Http(response.status().as_u16()).message());
        }
        response.bytes().map(|bytes| std::sync::Arc::new(bytes.to_vec()))
            .map_err(|_| "Выбранный снимок получен не полностью".into())
    }
    pub fn thumbnail_shared(&self, id: Uuid) -> Result<std::sync::Arc<Vec<u8>>> {
        if let Some(offline) = &self.offline {
            return offline.thumbnail_shared(id);
        }
        let (status, bytes, expected) = self.thumbnail_exchange(id)?;
        if status != 200 { return Err("Миниатюра пока недоступна".into()); }
        validate_thumbnail(&bytes, expected.as_deref())?;
        Ok(std::sync::Arc::new(bytes))
    }
    pub(crate) fn thumbnail_exchange(&self, id: Uuid) -> Result<(u16, Vec<u8>, Option<String>)> {
        use std::io::Read;
        let mut response = self.request(reqwest::Method::GET, &format!("/api/v1/assets/{id}/thumbnail"))
            .timeout(Duration::from_secs(15)).send()
            .map_err(|_| "Миниатюра еще не загружена; проверьте связь с API")?;
        let status = response.status().as_u16();
        let expected = response.headers().get("X-Content-SHA256").and_then(|v|v.to_str().ok()).map(str::to_owned);
        const MAX_THUMBNAIL: u64 = 1024 * 1024;
        if response.content_length().is_some_and(|size|size > MAX_THUMBNAIL) {
            return Err("Размер миниатюры превышает допустимый".into());
        }
        let mut bytes = Vec::new();
        response.by_ref().take(MAX_THUMBNAIL + 1).read_to_end(&mut bytes)
            .map_err(|_| "Миниатюра получена не полностью")?;
        if bytes.len() as u64 > MAX_THUMBNAIL { return Err("Размер миниатюры превышает допустимый".into()); }
        Ok((status, bytes, expected))
    }
    pub fn upload(&self, study: Uuid, kind: &str, bytes: Vec<u8>) -> Result<Asset> {
        if let Some(offline) = &self.offline {
            return offline.upload(study, kind, None, bytes);
        }
        let bytes = if kind == "original" {
            crate::imaging::storage_jpeg(bytes)?
        } else {
            bytes
        };
        let digest = hex::encode(Sha256::digest(&bytes));
        Self::decode(
            self.request(
                reqwest::Method::POST,
                &format!("/api/v1/studies/{study}/assets?kind={kind}&sha256={digest}"),
            )
            .body(bytes)
            .send(),
        )
    }
    pub fn upload_derived(&self, study: Uuid, parent: Uuid, bytes: Vec<u8>) -> Result<Asset> {
        if let Some(offline) = &self.offline {
            return offline.upload(study, "derived", Some(parent), bytes);
        }
        let digest = hex::encode(Sha256::digest(&bytes));
        Self::decode(
            self.request(
                reqwest::Method::POST,
                &format!(
                    "/api/v1/studies/{study}/assets?kind=derived&sha256={digest}&parent={parent}"
                ),
            )
            .body(bytes)
            .send(),
        )
    }
    /// Raw transport is used only by the background worker, never by an eframe callback.
    pub(crate) fn diagnosis_catalog_exchange(&self,path:&str)->Result<(u16,Vec<u8>)> {
        use std::io::Read;
        if path!="/api/v1/capabilities" && path!="/api/v1/diagnoses/catalog" && path!="/api/v1/diagnoses/priority" && !path.starts_with("/api/v1/diagnoses/catalog/page?") {
            return Err("Некорректный адрес каталога диагнозов".into());
        }
        let mut response=self.request(reqwest::Method::GET,path).timeout(Duration::from_secs(8)).send()
            .map_err(|_|"Справочник диагнозов временно недоступен")?;
        let status=response.status().as_u16();
        let limit=diagnosis_response_limit(path);
        if response.content_length().is_some_and(|size|size>limit){return Err("Ответ каталога диагнозов слишком большой".into());}
        let mut bytes=Vec::new();response.by_ref().take(limit+1).read_to_end(&mut bytes).map_err(|_|"Каталог диагнозов получен не полностью")?;
        if bytes.len() as u64>limit {return Err("Ответ каталога диагнозов слишком большой".into());}
        Ok((status,bytes))
    }
    pub fn exchange(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        bytes: Option<&[u8]>,
        operation: Option<&crate::offline::Operation>,
    ) -> Result<(u16, Vec<u8>)> {
        self.exchange_owned(method, path, body, bytes.map(<[u8]>::to_vec), operation)
    }
    /// The outbox already owns decrypted bytes. Move them into HTTP rather than
    /// copying another full original/PDF while preserving the exact request.
    pub(crate) fn exchange_owned(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
        bytes: Option<Vec<u8>>,
        operation: Option<&crate::offline::Operation>,
    ) -> Result<(u16, Vec<u8>)> {
        let method =
            reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| "Invalid method")?;
        let mut request = self.request(method, path);
        // Images can be large; their bounded transfer budget is independent of
        // short metadata requests, including when the workstation is on Wi-Fi.
        if path.starts_with("/api/v1/assets/") {
            request = request.timeout(Duration::from_secs(60));
        }
        if let Some(operation) = operation.filter(|op| !op.path.contains("/results/1c/pdf?")) {
            request = request
                .header("X-Operation-Id", operation.id.to_string())
                .header("X-Entity-Id", operation.entity_id.to_string());
            if let Some(version) = operation.version_id {
                request = request.header("X-Version-Id", version.to_string());
            }
        }
        if let Some(bytes) = bytes {
            if path.contains("/results/1c/pdf?") {
                request = request
                    .header("Content-Type", "application/pdf")
                    .header("X-Content-SHA256", hex::encode(Sha256::digest(&bytes)));
            }
            request = request
                .timeout(Duration::from_secs(60))
                .body(bytes);
        } else if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .map_err(|_| "Связь с API прервана; работа сохраняется локально")?;
        let status = response.status().as_u16();
        let bytes = response
            .bytes()
            .map_err(|_| "Ответ API потерян; операция остается в локальной очереди")?;
        Ok((status, bytes.to_vec()))
    }
}

pub(crate) fn diagnosis_response_limit(path: &str) -> u64 {
    if path.contains("/page?") { 12 * 1024 * 1024 } else if path == "/api/v1/diagnoses/priority" { 64 * 1024 } else { 16 * 1024 }
}

pub(crate) fn validate_thumbnail(bytes: &[u8], expected: Option<&str>) -> Result<String> {
    let sha256 = hex::encode(Sha256::digest(bytes));
    if expected != Some(sha256.as_str()) { return Err("SHA-256 миниатюры не совпадает с ответом сервера".into()); }
    let dimensions = image::io::Reader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Jpeg)
        .into_dimensions().map_err(|_| "Некорректная миниатюра JPEG")?;
    if dimensions.0 == 0 || dimensions.1 == 0 || dimensions.0 > 256 || dimensions.1 > 256 {
        return Err("Недопустимые размеры миниатюры".into());
    }
    image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
        .map_err(|_| "Повреждена миниатюра JPEG")?;
    Ok(sha256)
}

fn allowed_transport(url: &reqwest::Url) -> bool {
    if url.scheme() == "https" {
        return url.host().is_some();
    }
    if url.scheme() != "http" {
        return false;
    }
    let host = url.host_str().unwrap_or("");
    if host == "localhost" {
        return true;
    }
    match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            ip.is_loopback() || (cfg!(all(feature = "dev-http", not(feature = "standalone"))) && ip.is_private())
        }
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => false,
    }
}
#[cfg(test)]
mod transport_tests {
    use super::*;
    #[test]
    fn cloned_connections_reuse_the_same_keepalive_socket() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let connection = Connection::new(&format!("http://{}", listener.local_addr().unwrap()), "synthetic-key").unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
            for _ in 0..2 {
                let mut header = Vec::new();
                let mut byte = [0];
                while !header.ends_with(b"\r\n\r\n") {
                    socket.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                    assert!(header.len() < 16384);
                }
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\n{}").unwrap();
                socket.flush().unwrap();
            }
        });
        let _: serde_json::Value = connection.get("/first").unwrap();
        let _: serde_json::Value = connection.clone().get("/second").unwrap();
        server.join().unwrap();
    }
    #[cfg(feature = "standalone")]
    #[test]
    fn public_client_requires_https_for_lan_even_with_dev_http_enabled() {
        assert!(Connection::new("http://192.168.19.30:8787", "test-key").is_err());
        assert!(Connection::new("https://clinic.example.org:8787", "test-key").is_ok());
        assert!(Connection::new("http://127.0.0.1:8787", "test-key").is_ok());
    }
    #[test]
    fn test_http_is_explicit_and_does_not_disable_tls_validation() {
        for url in [
            "http://localhost:8787",
            "http://127.0.0.1:8787",
            "http://[::1]:8787",
            "https://example.com",
        ] {
            assert!(allowed_transport(&reqwest::Url::parse(url).unwrap()));
        }
        assert_eq!(
            allowed_transport(&reqwest::Url::parse("http://192.168.19.30:8787").unwrap()),
            cfg!(all(feature = "dev-http", not(feature = "standalone")))
        );
        for url in [
            "http://8.8.8.8",
            "http://example.com",
            "ftp://192.168.19.30",
        ] {
            assert!(!allowed_transport(&reqwest::Url::parse(url).unwrap()));
        }
        assert!(Connection::new("http://user:password@127.0.0.1:8787", "test").is_err());
        assert!(Connection::new("http://127.0.0.1:8787?key=secret", "test").is_err());
    }
}
