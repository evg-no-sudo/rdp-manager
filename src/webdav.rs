//! Immutable encrypted snapshots over WebDAV. Credentials never enter sync payloads.
use crate::transfer::{self, Result};
use reqwest::{
    blocking::{Client, Response},
    Method, StatusCode, Url,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    time::Duration,
};
use zeroize::Zeroizing;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub uid: String,
    pub directory: String,
    pub username: String,
}

impl Settings {
    pub fn new(directory: &str, username: &str) -> Result<Self> {
        let url = directory_url(directory)?;
        let username = username.trim();
        if username.is_empty() || username.contains(':') || username.chars().any(char::is_control) {
            return Err(
                "Введите имя пользователя WebDAV без двоеточия и управляющих символов.".into(),
            );
        }
        Ok(Self {
            uid: gtk::glib::uuid_string_random().into(),
            directory: url.to_string(),
            username: username.into(),
        })
    }

    pub fn load() -> Result<Option<Self>> {
        let path = gtk::glib::user_config_dir().join("rdp-manager/webdav.json");
        if !path.exists() {
            return Ok(None);
        }
        let file = std::fs::File::open(path).map_err(|_| "Не удалось открыть настройки WebDAV.")?;
        let mut bytes = Vec::new();
        file.take(16385)
            .read_to_end(&mut bytes)
            .map_err(|_| "Ошибка чтения настроек WebDAV.")?;
        if bytes.len() > 16384 {
            return Err("Файл настроек WebDAV слишком большой.".into());
        }
        let settings: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Некорректные настройки WebDAV.")?;
        directory_url(&settings.directory)?;
        if !gtk::glib::uuid_string_is_valid(&settings.uid) {
            return Err("Некорректный идентификатор настроек WebDAV.".into());
        }
        Self::new(&settings.directory, &settings.username)?;
        Ok(Some(settings))
    }

    pub fn save(&self) -> Result<()> {
        let directory = gtk::glib::user_config_dir().join("rdp-manager");
        std::fs::create_dir_all(&directory).map_err(|_| "Не удалось создать каталог настроек.")?;
        let mut file = tempfile::NamedTempFile::new_in(&directory)
            .map_err(|_| "Не удалось создать файл настроек.")?;
        let bytes =
            serde_json::to_vec_pretty(self).map_err(|_| "Не удалось подготовить настройки.")?;
        file.write_all(&bytes)
            .and_then(|_| file.as_file().sync_all())
            .map_err(|_| "Не удалось сохранить настройки.")?;
        file.persist(directory.join("webdav.json"))
            .map_err(|_| "Не удалось заменить файл настроек.")?;
        Ok(())
    }
}

fn directory_url(input: &str) -> Result<Url> {
    let mut url = Url::parse(input.trim()).map_err(|_| "Некорректный URL каталога WebDAV.")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Нужен HTTPS URL каталога без пароля, параметров и фрагмента. Имя пользователя указывается отдельно.".into());
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub name: String,
    pub modified: String,
}

pub struct Dav {
    client: Client,
    base: Url,
    username: String,
    password: Zeroizing<String>,
}

impl Dav {
    pub fn new(settings: Settings, password: Zeroizing<String>) -> Result<Self> {
        if password.is_empty() {
            return Err(
                "Введите пароль приложения WebDAV либо сохраните его в Secret Service.".into(),
            );
        }
        let base = directory_url(&settings.directory)?;
        let client = Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .user_agent("Linux-RDP-Manager/0.1")
            .build()
            .map_err(|_| "Не удалось создать HTTPS-клиент.")?;
        Ok(Self {
            client,
            base,
            username: settings.username,
            password,
        })
    }

    fn request(&self, method: Method, url: Url) -> reqwest::blocking::RequestBuilder {
        self.client
            .request(method, url)
            .basic_auth(&self.username, Some(self.password.as_str()))
    }

    pub fn list(&self) -> Result<Vec<Snapshot>> {
        let response = self.request(Method::from_bytes(b"PROPFIND").unwrap(), self.base.clone())
            .header("Depth", "1").header("Content-Type", "application/xml; charset=utf-8")
            .body(r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getlastmodified/></d:prop></d:propfind>"#)
            .send().map_err(network_error)?;
        if response.status() != StatusCode::MULTI_STATUS {
            return Err(status_error(response.status()));
        }
        let bytes = bounded(response, 2 * 1024 * 1024)?;
        parse_listing(&self.base, &bytes)
    }

    pub fn upload(&self, bytes: Vec<u8>) -> Result<String> {
        if bytes.len() > transfer::MAX_BYTES {
            return Err("Набор превышает 4 МиБ.".into());
        }
        let timestamp = gtk::glib::DateTime::now_utc()
            .and_then(|time| time.format("%Y%m%dT%H%M%SZ"))
            .map_err(|_| "Не удалось определить время для имени набора.")?;
        let name = format!(
            "rdp-profiles-{timestamp}-{}.rdpm",
            gtk::glib::uuid_string_random()
        );
        self.put(&name, bytes)?;
        Ok(name)
    }

    fn put(&self, name: &str, bytes: Vec<u8>) -> Result<()> {
        let response = self
            .request(Method::PUT, self.target(name)?)
            .header("If-None-Match", "*")
            .header("Content-Type", "application/octet-stream")
            .body(bytes)
            .send()
            .map_err(network_error)?;
        if !matches!(
            response.status(),
            StatusCode::CREATED | StatusCode::NO_CONTENT | StatusCode::OK
        ) {
            return Err(status_error(response.status()));
        }
        Ok(())
    }

    pub fn download(&self, name: &str) -> Result<Vec<u8>> {
        let response = self
            .request(Method::GET, self.target(name)?)
            .send()
            .map_err(network_error)?;
        if response.status() != StatusCode::OK {
            return Err(status_error(response.status()));
        }
        bounded(response, transfer::MAX_BYTES)
    }

    fn target(&self, name: &str) -> Result<Url> {
        if !snapshot_name(name) {
            return Err("Некорректное имя набора.".into());
        }
        self.base
            .join(name)
            .map_err(|_| "Некорректный адрес набора.".into())
    }
}

fn snapshot_name(name: &str) -> bool {
    name.starts_with("rdp-profiles-")
        && name.ends_with(".rdpm")
        && name.len() <= 120
        && name[..name.len() - 5]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn bounded(response: Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("Ответ сервера превышает допустимый размер.".into());
    }
    let mut bytes = Vec::new();
    response
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "Не удалось полностью прочитать ответ WebDAV.")?;
    if bytes.len() > limit {
        return Err("Ответ сервера превышает допустимый размер.".into());
    }
    Ok(bytes)
}

fn network_error(error: reqwest::Error) -> String {
    // reqwest errors may include a private URL; never display/debug the raw error.
    if error.is_timeout() {
        "Сервер WebDAV не ответил за отведённое время.".into()
    } else {
        "Не удалось подключиться к WebDAV. Проверьте адрес, сеть и доверенный HTTPS-сертификат. Перенаправления не выполняются.".into()
    }
}

fn status_error(status: StatusCode) -> String {
    match status.as_u16() {
        401 => "WebDAV: неверное имя пользователя или пароль приложения.".into(),
        403 => "WebDAV: недостаточно прав на выбранный каталог.".into(),
        404 | 409 => {
            "Каталог WebDAV не найден. Создайте папку в Nextcloud и укажите её точный WebDAV URL."
                .into()
        }
        412 => "Набор с таким именем уже существует. Он не перезаписан; повторите отправку.".into(),
        423 => "Каталог WebDAV временно заблокирован. Повторите позже.".into(),
        507 => "В хранилище недостаточно свободного места.".into(),
        _ if status.is_redirection() => {
            "WebDAV вернул перенаправление. Укажите конечный HTTPS URL каталога.".into()
        }
        _ => format!("Ошибка WebDAV: HTTP {}.", status.as_u16()),
    }
}

fn parse_listing(base: &Url, bytes: &[u8]) -> Result<Vec<Snapshot>> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Ответ WebDAV не в UTF-8.")?;
    let document =
        roxmltree::Document::parse(text).map_err(|_| "Некорректный XML-ответ WebDAV.")?;
    if !document
        .root_element()
        .has_tag_name(("DAV:", "multistatus"))
    {
        return Err("Ответ не является списком WebDAV.".into());
    }
    let mut result = Vec::new();
    let mut names = std::collections::HashSet::new();
    for response in document
        .root_element()
        .children()
        .filter(|node| node.has_tag_name(("DAV:", "response")))
    {
        let Some(href) = response
            .children()
            .find(|node| node.has_tag_name(("DAV:", "href")))
            .and_then(|node| node.text())
        else {
            continue;
        };
        let Ok(url) = base.join(href.trim()) else {
            continue;
        };
        if url.origin() != base.origin()
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            continue;
        }
        let Some(name) = url.path().strip_prefix(base.path()) else {
            continue;
        };
        if !snapshot_name(name) || !names.insert(name.to_owned()) {
            continue;
        }
        for propstat in response
            .children()
            .filter(|node| node.has_tag_name(("DAV:", "propstat")))
        {
            let status = propstat
                .children()
                .find(|node| node.has_tag_name(("DAV:", "status")))
                .and_then(|node| node.text())
                .unwrap_or("");
            if status.split_whitespace().nth(1) != Some("200") {
                continue;
            }
            let Some(prop) = propstat
                .children()
                .find(|node| node.has_tag_name(("DAV:", "prop")))
            else {
                continue;
            };
            if prop
                .descendants()
                .any(|node| node.has_tag_name(("DAV:", "collection")))
            {
                continue;
            }
            let modified = prop
                .children()
                .find(|node| node.has_tag_name(("DAV:", "getlastmodified")))
                .and_then(|node| node.text())
                .unwrap_or("Время не указано");
            result.push(Snapshot {
                name: name.into(),
                modified: modified.chars().take(100).collect(),
            });
            break;
        }
        if result.len() > 2000 {
            return Err("В каталоге более 2000 наборов. Используйте отдельную папку.".into());
        }
    }
    result.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_and_schema_never_accept_inline_credentials_or_insecure_urls() {
        for input in [
            "http://example.test/dav/",
            "https://u:p@example.test/dav",
            "https://example.test/dav?token=secret",
            "https://example.test/#x",
            "file:///tmp/dav",
        ] {
            assert!(Settings::new(input, "alice").is_err());
        }
        let config = Settings::new(
            "https://example.test/remote.php/dav/files/alice/RdpManager",
            "alice",
        )
        .unwrap();
        assert!(config.directory.ends_with('/'));
        let json = serde_json::to_value(config).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 3);
        assert!(json.get("password").is_none());
        for name in [
            "../rdp-profiles-a.rdpm",
            "rdp-profiles-a/b.rdpm",
            "rdp-profiles-%2f.rdpm",
            "rdp-profiles-a.rdpm?x",
            "secret.txt",
        ] {
            assert!(!snapshot_name(name));
        }
    }

    #[test]
    fn listing_filters_untrusted_origins_paths_and_failed_properties() {
        let base = Url::parse("https://example.test/dav/folder/").unwrap();
        let xml = br#"<d:multistatus xmlns:d="DAV:">
            <d:response><d:href>/dav/folder/rdp-profiles-good.rdpm</d:href><d:propstat><d:prop><d:getlastmodified>today</d:getlastmodified></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>
            <d:response><d:href>https://other.test/dav/folder/rdp-profiles-other.rdpm</d:href></d:response>
            <d:response><d:href>/else/rdp-profiles-secret.rdpm</d:href></d:response>
            <d:response><d:href>/dav/folder/rdp-profiles-sub/rdp-profiles-a.rdpm</d:href></d:response>
            <d:response><d:href>/dav/folder/rdp-profiles-bad.rdpm</d:href><d:propstat><d:prop/><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response>
            </d:multistatus>"#;
        let list = parse_listing(&base, xml).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "rdp-profiles-good.rdpm");
        assert!(parse_listing(&base, b"<html/>").is_err());
        assert!(parse_listing(&base, br#"<!DOCTYPE x [<!ENTITY x "secret">]><x>&x;</x>"#).is_err());
    }

    #[test]
    #[ignore = "Requires loopback sockets; run cargo test webdav_http -- --ignored"]
    fn webdav_http_transport_roundtrip_and_errors() {
        use std::net::TcpListener;
        let payload = transfer::encrypt(
            vec![crate::transfer::PortableProfile::from(
                &crate::store::Profile {
                    name: "Test".into(),
                    host: "server.test".into(),
                    ..Default::default()
                },
            )],
            "synthetic-master-password",
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = Url::parse(&format!("http://{}/dav/", listener.local_addr().unwrap())).unwrap();
        let expected = payload.clone();
        let server = std::thread::spawn(move || {
            for (method, status, headers, body) in [
                ("PROPFIND", 207, "", br#"<d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/rdp-profiles-test.rdpm</d:href><d:propstat><d:prop/><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#.to_vec()),
                ("PUT", 201, "", Vec::new()),
                ("GET", 200, "", expected.clone()),
                ("PUT", 412, "", Vec::new()),
                ("PROPFIND", 401, "", Vec::new()),
                ("PROPFIND", 302, "Location: https://untrusted.example/\r\n", Vec::new()),
                ("GET", 200, "X-Test: oversized\r\n", Vec::new()),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0]; stream.read_exact(&mut byte).unwrap(); request.push(byte[0]);
                    assert!(request.len() < 32768);
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with(method));
                let lower = request.to_ascii_lowercase();
                assert!(lower.contains("authorization: basic "));
                let length = lower.lines().find_map(|line| line.strip_prefix("content-length: ")).map(|s| s.trim().parse::<usize>().unwrap()).unwrap_or(0);
                let mut uploaded = vec![0; length]; stream.read_exact(&mut uploaded).unwrap();
                if method == "PUT" {
                    assert!(lower.contains("if-none-match: *"));
                    assert!(uploaded == expected);
                }
                let length = if headers.contains("oversized") { transfer::MAX_BYTES + 1 } else { body.len() };
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {length}\r\nConnection: close\r\n{headers}\r\n").unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        // HTTP bypass exists only inside this test fixture; Dav::new always requires HTTPS.
        let dav = Dav {
            client: Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
            base,
            username: "test-user".into(),
            password: Zeroizing::new("synthetic-test-token".into()),
        };
        assert_eq!(dav.list().unwrap().len(), 1);
        dav.put("rdp-profiles-test.rdpm", payload.clone()).unwrap();
        assert!(dav.download("rdp-profiles-test.rdpm").unwrap() == payload);
        assert!(dav
            .put("rdp-profiles-test.rdpm", payload)
            .unwrap_err()
            .contains("не перезаписан"));
        assert!(dav.list().unwrap_err().contains("пароль приложения"));
        assert!(dav.list().unwrap_err().contains("перенаправление"));
        assert!(dav
            .download("rdp-profiles-test.rdpm")
            .unwrap_err()
            .contains("размер"));
        server.join().unwrap();
    }
}
