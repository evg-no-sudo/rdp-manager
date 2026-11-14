use crate::transfer::{self, Snapshot};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ssh2::{CheckResult, ErrorCode, KnownHostFileKind, OpenFlags, OpenType, RenameFlags, Session};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    Agent,
    KeyFile,
    Password,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub uid: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub directory: String,
    pub auth: Auth,
    pub key_file: String,
    pub secret_saved: bool,
}

impl Settings {
    pub fn validate(&mut self) -> transfer::Result<()> {
        self.host = self
            .host
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase();
        self.username = self.username.trim().into();
        self.directory = self.directory.trim().into();
        if self.host.is_empty()
            || self.host.len() > 253
            || self
                .host
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "/@\\[]".contains(c))
            || self.port == 0
        {
            return Err("Введите DNS-имя или IP сервера без схемы; порт — от 1 до 65535.".into());
        }
        if self.host.contains(':') && self.host.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(
                "Порт указывается в отдельном поле. В поле сервера допустимы DNS, IPv4 или IPv6."
                    .into(),
            );
        }
        if self.username.is_empty()
            || self.username.len() > 1024
            || self.username.chars().any(char::is_control)
        {
            return Err("Введите корректного пользователя SSH.".into());
        }
        if self.directory.is_empty()
            || self.directory.len() > 4096
            || self.directory.chars().any(char::is_control)
            || Path::new(&self.directory)
                .components()
                .any(|c| c == Component::ParentDir)
            || self.directory.starts_with('~')
        {
            return Err("Укажите каталог без '..' и '~': абсолютный путь или путь относительно домашнего каталога SFTP (например rdp-manager).".into());
        }
        if self.auth == Auth::KeyFile {
            if self.key_file.len() > 4096
                || !Path::new(&self.key_file).is_absolute()
                || self.key_file.chars().any(char::is_control)
            {
                return Err("Выберите абсолютный путь к файлу приватного SSH-ключа.".into());
            }
        } else {
            self.key_file.clear();
        }
        if self.auth == Auth::Agent {
            self.secret_saved = false;
        }
        if !gtk::glib::uuid_string_is_valid(&self.uid) {
            return Err("Некорректный идентификатор настроек SFTP.".into());
        }
        Ok(())
    }

    pub fn same_login(&self, other: &Self) -> bool {
        self.host == other.host
            && self.port == other.port
            && self.username == other.username
            && self.auth == other.auth
            && self.key_file == other.key_file
    }

    pub fn same_location(&self, other: &Self) -> bool {
        self.same_login(other) && self.directory == other.directory
    }

    pub fn load() -> transfer::Result<Option<Self>> {
        let path = gtk::glib::user_config_dir().join("rdp-manager/sftp.json");
        let file = match std::fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Не удалось открыть настройки SFTP.".into()),
        };
        let mut bytes = Vec::new();
        file.take(32769)
            .read_to_end(&mut bytes)
            .map_err(|_| "Ошибка чтения настроек SFTP.")?;
        if bytes.len() > 32768 {
            return Err("Файл настроек SFTP слишком большой.".into());
        }
        let mut settings: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Некорректные настройки SFTP.")?;
        settings.validate()?;
        Ok(Some(settings))
    }

    pub fn save(&self) -> transfer::Result<()> {
        let mut checked = self.clone();
        checked.validate()?;
        let directory = gtk::glib::user_config_dir().join("rdp-manager");
        std::fs::create_dir_all(&directory).map_err(|_| "Не удалось создать каталог настроек.")?;
        let mut file = tempfile::NamedTempFile::new_in(&directory)
            .map_err(|_| "Не удалось создать настройки SFTP.")?;
        let bytes = serde_json::to_vec_pretty(&checked)
            .map_err(|_| "Не удалось подготовить настройки SFTP.")?;
        file.write_all(&bytes)
            .and_then(|_| file.as_file().sync_all())
            .map_err(|_| "Не удалось записать настройки SFTP.")?;
        file.persist(directory.join("sftp.json"))
            .map_err(|_| "Не удалось сохранить настройки SFTP.")?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct HostIdentity {
    pub host: String,
    pub port: u16,
    pub key: Vec<u8>,
    pub fingerprint: String,
    pub algorithm: String,
}

#[derive(Debug)]
pub enum Failure {
    Message(String),
    UnknownHost(HostIdentity),
}
impl From<String> for Failure {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}
impl From<&str> for Failure {
    fn from(value: &str) -> Self {
        Self::Message(value.into())
    }
}
pub type Result<T> = std::result::Result<T, Failure>;

pub struct Remote {
    sftp: ssh2::Sftp,
    directory: PathBuf,
}

impl Remote {
    pub fn connect(
        settings: Settings,
        secret: Zeroizing<String>,
        trusted: Option<Vec<u8>>,
    ) -> Result<Self> {
        Self::connect_inner(
            settings,
            secret,
            trusted,
            &gtk::glib::home_dir().join(".ssh/known_hosts"),
        )
    }

    fn connect_inner(
        mut settings: Settings,
        secret: Zeroizing<String>,
        trusted: Option<Vec<u8>>,
        known_hosts: &Path,
    ) -> Result<Self> {
        settings.validate()?;
        let addresses: Vec<_> = (settings.host.as_str(), settings.port)
            .to_socket_addrs()
            .map_err(|_| "Не удалось определить адрес SSH-сервера.")?
            .take(4)
            .collect();
        let tcp = addresses
            .iter()
            .find_map(|address| TcpStream::connect_timeout(address, Duration::from_secs(10)).ok())
            .ok_or("Не удалось подключиться к SSH-серверу. Проверьте адрес, порт и сеть.")?;
        tcp.set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|_| "Не удалось настроить SSH-соединение.")?;
        tcp.set_write_timeout(Some(Duration::from_secs(30)))
            .map_err(|_| "Не удалось настроить SSH-соединение.")?;
        let mut session = Session::new().map_err(|_| "Не удалось создать SSH-сессию.")?;
        session.set_timeout(30_000);
        session.set_tcp_stream(tcp);
        session
            .handshake()
            .map_err(|_| "SSH handshake не выполнен. Проверьте совместимость SSH-сервера.")?;
        let (key, algorithm) = session
            .host_key()
            .ok_or("Сервер не предоставил ключ SSH.")?;
        let identity = HostIdentity {
            host: settings.host.clone(),
            port: settings.port,
            key: key.to_vec(),
            fingerprint: format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(key))),
            algorithm: format!("{algorithm:?}"),
        };
        if trusted.as_ref().is_some_and(|pinned| pinned != key) {
            return Err("Ключ SSH-сервера изменился! Авторизация не выполнялась. Проверьте сервер и его новый отпечаток у администратора.".into());
        }
        let mut known = session
            .known_hosts()
            .map_err(|_| "Не удалось проверить SSH known_hosts.")?;
        match std::fs::metadata(known_hosts) {
            Ok(_) => {
                known
                    .read_file(known_hosts, KnownHostFileKind::OpenSSH)
                    .map_err(|_| {
                        "Не удалось прочитать SSH known_hosts; авторизация остановлена."
                    })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("SSH known_hosts недоступен; авторизация остановлена.".into()),
        }
        match known.check_port(&settings.host, settings.port, key) {
            CheckResult::Match => {}
            CheckResult::Mismatch => {
                return Err(
                    "Ключ SSH-сервера отличается от known_hosts! Авторизация не выполнялась."
                        .into(),
                )
            }
            CheckResult::Failure => return Err("Не удалось проверить ключ SSH-сервера.".into()),
            CheckResult::NotFound if trusted.is_some() => {}
            CheckResult::NotFound => return Err(Failure::UnknownHost(identity)),
        }
        match settings.auth {
            Auth::Agent => {
                let mut agent = session.agent().map_err(|_| "SSH-agent недоступен.")?;
                agent
                    .connect()
                    .map_err(|_| "Не удалось подключиться к SSH-agent. Проверьте SSH_AUTH_SOCK.")?;
                agent
                    .list_identities()
                    .map_err(|_| "Не удалось получить список ключей SSH-agent.")?;
                let identities = agent
                    .identities()
                    .map_err(|_| "Не удалось прочитать ключи SSH-agent.")?;
                for identity in identities.iter().take(16) {
                    if agent.userauth(&settings.username, identity).is_ok() {
                        break;
                    }
                }
                if !session.authenticated() {
                    return Err("Вход через SSH-agent не выполнен. Проверьте добавленные ключи и authorized_keys на сервере.".into());
                }
            }
            Auth::KeyFile => {
                if !Path::new(&settings.key_file).is_file() {
                    return Err("Файл приватного SSH-ключа не найден.".into());
                }
                session.userauth_pubkey_file(&settings.username, None, Path::new(&settings.key_file), if secret.is_empty(){None}else{Some(&secret)})
                    .map_err(|_| "Вход по SSH-ключу не выполнен. Проверьте ключ, passphrase, формат ключа и authorized_keys на сервере.")?;
            }
            Auth::Password => {
                if secret.is_empty() {
                    return Err(
                        "Введите пароль SSH или используйте сохранённый локальный пароль.".into(),
                    );
                }
                session.userauth_password(&settings.username, &secret).map_err(|_| "Вход по паролю SSH не выполнен. Проверьте пользователя и пароль; сервер должен разрешать этот метод.")?;
            }
        }
        if !session.authenticated() {
            return Err("Сервер не подтвердил SSH-аутентификацию.".into());
        }
        let sftp = session
            .sftp()
            .map_err(|_| "Сервер не предоставил подсистему SFTP.")?;
        let directory = PathBuf::from(settings.directory);
        if !sftp
            .stat(&directory)
            .map_err(|_| "Каталог SFTP недоступен. Создайте папку на сервере и проверьте права.")?
            .is_dir()
        {
            return Err("Указанный путь SFTP не является каталогом.".into());
        }
        Ok(Self { sftp, directory })
    }

    pub fn list(&self) -> Result<Vec<Snapshot>> {
        let mut handle = self
            .sftp
            .opendir(&self.directory)
            .map_err(|_| "Не удалось открыть каталог SFTP.")?;
        let mut result = Vec::new();
        for _ in 0..20_000 {
            let (path, stat) = match handle.readdir() {
                Ok(entry) => entry,
                Err(error) if error.code() == ErrorCode::Session(-16) => {
                    result.sort_by(|a: &Snapshot, b| b.name.cmp(&a.name));
                    return Ok(result);
                }
                Err(_) => return Err("Не удалось прочитать каталог SFTP.".into()),
            };
            let Some(name) = path.to_str() else {
                continue;
            };
            if !transfer::snapshot_name(name) || !stat.is_file() {
                continue;
            }
            let modified = stat
                .mtime
                .and_then(|mtime| i64::try_from(mtime).ok())
                .and_then(|mtime| gtk::glib::DateTime::from_unix_utc(mtime).ok())
                .and_then(|time| time.format("%Y-%m-%d %H:%M UTC").ok())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "Время не указано".into());
            result.push(Snapshot {
                name: name.into(),
                modified,
            });
            if result.len() > 2000 {
                return Err("В каталоге более 2000 наборов. Используйте отдельную папку.".into());
            }
        }
        Err("В каталоге слишком много файлов. Используйте отдельную папку для наборов.".into())
    }

    pub fn upload(&self, bytes: Vec<u8>) -> Result<String> {
        let name = transfer::new_snapshot_name()?;
        self.put(&name, &bytes)?;
        Ok(name)
    }

    fn put(&self, name: &str, bytes: &[u8]) -> Result<()> {
        if !transfer::snapshot_name(name) || bytes.len() > transfer::MAX_BYTES {
            return Err("Некорректное имя или размер набора.".into());
        }
        let temporary = self.directory.join(format!(
            ".rdp-manager-{}.tmp",
            gtk::glib::uuid_string_random()
        ));
        let destination = self.directory.join(name);
        let result = (|| {
            let mut file = self
                .sftp
                .open_mode(
                    &temporary,
                    OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUSIVE,
                    0o600,
                    OpenType::File,
                )
                .map_err(|_| {
                    "Не удалось создать временный файл SFTP. Проверьте права и свободное место."
                })?;
            file.write_all(bytes)
                .map_err(|_| "Не удалось полностью отправить набор SFTP.")?;
            if let Err(error) = file.fsync() {
                if error.code() != ErrorCode::SFTP(8) {
                    return Err("Не удалось подтвердить запись набора SFTP.".into());
                }
            }
            file.close()
                .map_err(|_| "Не удалось закрыть отправленный файл SFTP.")?;
            self.sftp
                .rename(&temporary, &destination, Some(RenameFlags::empty()))
                .map_err(|_| "Не удалось опубликовать набор SFTP. Существующий файл не заменён.")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.sftp.unlink(&temporary);
        }
        result
    }

    pub fn download(&self, name: &str) -> Result<Vec<u8>> {
        if !transfer::snapshot_name(name) {
            return Err("Некорректное имя набора.".into());
        }
        let path = self.directory.join(name);
        let stat = self
            .sftp
            .lstat(&path)
            .map_err(|_| "Набор SFTP недоступен.")?;
        if !stat.is_file()
            || stat
                .size
                .is_some_and(|size| size > transfer::MAX_BYTES as u64)
        {
            return Err("Набор SFTP не является обычным файлом или превышает 4 МиБ.".into());
        }
        let file = self
            .sftp
            .open(&path)
            .map_err(|_| "Не удалось открыть набор SFTP.")?;
        let mut bytes = Vec::new();
        file.take((transfer::MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "Не удалось прочитать набор SFTP.")?;
        if bytes.len() > transfer::MAX_BYTES {
            return Err("Набор SFTP превышает 4 МиБ.".into());
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            uid: gtk::glib::uuid_string_random().into(),
            host: "example.test".into(),
            port: 22,
            username: "alice".into(),
            directory: "rdp-manager".into(),
            auth: Auth::Agent,
            key_file: String::new(),
            secret_saved: false,
        }
    }

    #[test]
    fn validation_and_local_config_never_contain_key_contents_or_passwords() {
        let mut config = settings();
        config.validate().unwrap();
        for host in [
            "",
            "sftp://example.test",
            "user@example.test",
            "bad host",
            "example.test/path",
        ] {
            let mut bad = config.clone();
            bad.host = host.into();
            assert!(bad.validate().is_err());
        }
        for path in ["", "../other", "/folder/../other", "~/rdp-manager"] {
            let mut bad = config.clone();
            bad.directory = path.into();
            assert!(bad.validate().is_err());
        }
        let mut key = config.clone();
        key.auth = Auth::KeyFile;
        assert!(key.validate().is_err());
        key.key_file = "/tmp/synthetic-key".into();
        key.validate().unwrap();
        assert!(!key.same_login(&config));
        let mut other = key.clone();
        other.key_file = "/tmp/other-key".into();
        assert!(!other.same_login(&key));
        let json = serde_json::to_value(&key).unwrap();
        let fields: std::collections::BTreeSet<_> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            [
                "uid",
                "host",
                "port",
                "username",
                "directory",
                "auth",
                "key_file",
                "secret_saved"
            ]
            .into_iter()
            .collect()
        );
        let mut injected = json;
        injected["private_key"] = "must-not-store".into();
        assert!(serde_json::from_value::<Settings>(injected).is_err());
    }

    #[test]
    #[ignore = "Run scripts/test-sftp.sh with an isolated localhost SSH server"]
    fn sftp_live_keys_agent_trust_and_encrypted_exchange() {
        let root = PathBuf::from(
            std::env::var("RDP_MANAGER_SFTP_FIXTURE").expect("Use scripts/test-sftp.sh"),
        );
        assert!(root.to_string_lossy().starts_with("/tmp/rdp-manager-sftp."));
        let mut config = settings();
        config.host = "127.0.0.1".into();
        config.port = std::env::var("RDP_MANAGER_SFTP_PORT")
            .unwrap()
            .parse()
            .unwrap();
        config.username = std::env::var("RDP_MANAGER_SFTP_USER").unwrap();
        config.directory = root.join("store").to_string_lossy().into();
        config.auth = Auth::KeyFile;
        config.key_file = root.join("client").to_string_lossy().into();
        let known = root.join("known_hosts");
        let identity = match Remote::connect_inner(
            config.clone(),
            Zeroizing::new(String::new()),
            None,
            &known,
        ) {
            Err(Failure::UnknownHost(identity)) => identity,
            _ => panic!("An unknown server must not authenticate"),
        };
        let public = std::fs::read_to_string(root.join("host.pub")).unwrap();
        let expected = base64::engine::general_purpose::STANDARD
            .decode(public.split_whitespace().nth(1).unwrap())
            .unwrap();
        assert_eq!(identity.key, expected);
        assert!(identity.fingerprint.starts_with("SHA256:"));
        let mut changed = expected.clone();
        changed[0] ^= 1;
        assert!(matches!(
            Remote::connect_inner(
                config.clone(),
                Zeroizing::new(String::new()),
                Some(changed),
                &known
            ),
            Err(Failure::Message(_))
        ));
        let remote = Remote::connect_inner(
            config.clone(),
            Zeroizing::new(String::new()),
            Some(expected.clone()),
            &known,
        )
        .unwrap();
        let profiles = vec![transfer::PortableProfile::from(&crate::store::Profile {
            name: "Office".into(),
            host: "server.test".into(),
            ..Default::default()
        })];
        let bytes = transfer::encrypt(profiles.clone(), "synthetic-master-password").unwrap();
        let name = remote.upload(bytes.clone()).unwrap();
        assert_eq!(remote.list().unwrap().len(), 1);
        let downloaded = remote.download(&name).unwrap();
        assert!(downloaded == bytes);
        assert_eq!(
            transfer::decrypt(&downloaded, "synthetic-master-password").unwrap(),
            profiles
        );
        assert!(remote.put(&name, b"do-not-overwrite").is_err());
        assert!(remote.download(&name).unwrap() == bytes);
        assert!(remote.download("../private.rdpm").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("store").join(&name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            std::os::unix::fs::symlink(
                root.join("store").join(&name),
                root.join("store/rdp-profiles-link.rdpm"),
            )
            .unwrap();
            assert!(remote.download("rdp-profiles-link.rdpm").is_err());
            assert_eq!(remote.list().unwrap().len(), 1);
        }
        let big = std::fs::File::create(root.join("store/rdp-profiles-big.rdpm")).unwrap();
        big.set_len((transfer::MAX_BYTES + 1) as u64).unwrap();
        assert!(remote.download("rdp-profiles-big.rdpm").is_err());
        drop(remote);
        config.key_file = root.join("encrypted").to_string_lossy().into();
        assert!(Remote::connect_inner(
            config.clone(),
            Zeroizing::new("wrong-passphrase".into()),
            Some(expected.clone()),
            &known
        )
        .is_err());
        let encrypted = Remote::connect_inner(
            config.clone(),
            Zeroizing::new("synthetic-key-passphrase".into()),
            Some(expected.clone()),
            &known,
        )
        .unwrap();
        assert!(encrypted.download(&name).unwrap() == bytes);
        drop(encrypted);
        config.auth = Auth::Agent;
        config.key_file.clear();
        let agent = Remote::connect_inner(
            config.clone(),
            Zeroizing::new(String::new()),
            Some(expected.clone()),
            &known,
        )
        .unwrap();
        assert!(agent.download(&name).unwrap() == bytes);
        drop(agent);
        std::fs::write(
            &known,
            format!(
                "[127.0.0.1]:{} {} {}\n",
                config.port,
                public.split_whitespace().next().unwrap(),
                public.split_whitespace().nth(1).unwrap()
            ),
        )
        .unwrap();
        assert!(
            Remote::connect_inner(config.clone(), Zeroizing::new(String::new()), None, &known)
                .is_ok()
        );
        let wrong_public = std::fs::read_to_string(root.join("client.pub")).unwrap();
        std::fs::write(
            &known,
            format!(
                "[127.0.0.1]:{} {} {}\n",
                config.port,
                wrong_public.split_whitespace().next().unwrap(),
                wrong_public.split_whitespace().nth(1).unwrap()
            ),
        )
        .unwrap();
        assert!(matches!(
            Remote::connect_inner(
                config,
                Zeroizing::new(String::new()),
                Some(expected),
                &known
            ),
            Err(Failure::Message(_))
        ));
    }
}
