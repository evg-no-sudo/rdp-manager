//! Portable, versioned allowlist. No credentials or local database IDs are serializable.
use crate::store::Profile;
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{rand_core::RngCore, Aead, KeyInit, OsRng, Payload},
    XChaCha20Poly1305, XNonce,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

pub type Result<T> = std::result::Result<T, String>;
const MAGIC: &[u8; 8] = b"RDPMENC\x01";
const HEADER: usize = 8 + 16 + 24;
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
pub fn snapshot_name(name: &str) -> bool {
    name.starts_with("rdp-profiles-")
        && name.ends_with(".rdpm")
        && name.len() <= 120
        && name[..name.len() - 5]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub fn new_snapshot_name() -> Result<String> {
    let timestamp = gtk::glib::DateTime::now_utc()
        .and_then(|time| time.format("%Y%m%dT%H%M%SZ"))
        .map_err(|_| {
            tr!(
                "Не удалось определить время для имени набора.",
                "Could not generate the bundle timestamp."
            )
        })?;
    Ok(format!(
        "rdp-profiles-{timestamp}-{}.rdpm",
        gtk::glib::uuid_string_random()
    ))
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub name: String,
    pub modified: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableProfile {
    pub uid: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

impl From<&Profile> for PortableProfile {
    fn from(p: &Profile) -> Self {
        Self {
            uid: p.uid.clone(),
            name: p.name.clone(),
            host: p.host.clone(),
            port: p.port,
            username: p.username.clone(),
            domain: p.domain.clone(),
            folder: None,
        }
    }
}

impl PortableProfile {
    pub fn validate_folder(&self) -> Result<()> {
        if self
            .folder
            .as_deref()
            .is_some_and(|name| name.chars().count() > 80 || name.chars().any(char::is_control))
        {
            return Err(tr!("Некорректное название папки.", "Invalid folder name.").into());
        }
        Ok(())
    }

    pub fn profile(&self) -> Profile {
        Profile {
            id: None,
            uid: self.uid.clone(),
            name: self.name.clone(),
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            domain: self.domain.clone(),
        }
    }

    pub fn same_fields(&self, local: &Profile) -> bool {
        self.name == local.name
            && self.host.eq_ignore_ascii_case(&local.host)
            && self.port == local.port
            && self.username == local.username
            && self.domain == local.domain
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    version: u32,
    profiles: Vec<PortableProfile>,
}

fn validate(profiles: &[PortableProfile]) -> Result<()> {
    if profiles.len() > 10_000 {
        return Err(tr!(
            "В наборе более 10000 профилей.",
            "The bundle contains more than 10,000 profiles."
        )
        .into());
    }
    let mut ids = HashSet::new();
    for p in profiles {
        p.validate_folder()?;
        p.profile().validate().map_err(|_| {
            tr!(
                "Набор содержит некорректный профиль.",
                "The bundle contains an invalid profile."
            )
        })?;
        if !ids.insert(&p.uid) {
            return Err(tr!(
                "В наборе повторяются идентификаторы профилей.",
                "The bundle contains duplicate profile identifiers."
            )
            .into());
        }
        if [&p.uid, &p.name, &p.host, &p.username, &p.domain]
            .iter()
            .any(|s| s.len() > 4096)
        {
            return Err(tr!(
                "Поле профиля превышает допустимую длину.",
                "A profile field is too long."
            )
            .into());
        }
    }
    Ok(())
}

fn key(password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    if password.is_empty() || password.len() > 1024 {
        return Err(tr!(
            "Введите мастер-пароль длиной до 1024 байт.",
            "Enter a master password of up to 1,024 bytes."
        )
        .into());
    }
    // Fixed by the authenticated format version; untrusted input cannot inflate KDF costs.
    let params = Params::new(65536, 3, 1, Some(32)).map_err(|_| {
        tr!(
            "Ошибка параметров шифрования.",
            "Invalid encryption parameters."
        )
    })?;
    let mut key = Zeroizing::new([0; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, key.as_mut())
        .map_err(|_| {
            tr!(
                "Не удалось получить ключ шифрования.",
                "Could not derive the encryption key."
            )
        })?;
    Ok(key)
}

pub fn encrypt(profiles: Vec<PortableProfile>, password: &str) -> Result<Vec<u8>> {
    if password.chars().count() < 12 {
        return Err(tr!(
            "Мастер-пароль должен содержать минимум 12 символов.",
            "The master password must contain at least 12 characters."
        )
        .into());
    }
    validate(&profiles)?;
    let plaintext = Zeroizing::new(
        serde_json::to_vec(&Bundle {
            version: 1,
            profiles,
        })
        .map_err(|_| {
            tr!(
                "Не удалось подготовить набор.",
                "Could not prepare the bundle."
            )
        })?,
    );
    if plaintext.len() > MAX_BYTES - HEADER - 16 {
        return Err(tr!("Набор превышает 4 МиБ.", "The bundle exceeds 4 MiB.").into());
    }
    let mut header = [0u8; HEADER];
    header[..8].copy_from_slice(MAGIC);
    OsRng.try_fill_bytes(&mut header[8..]).map_err(|_| {
        tr!(
            "Системный генератор случайных чисел недоступен.",
            "The system random number generator is unavailable."
        )
    })?;
    let key = key(password, &header[8..24])?;
    let cipher = XChaCha20Poly1305::new((&*key).into());
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&header[24..]),
            Payload {
                msg: &plaintext,
                aad: &header,
            },
        )
        .map_err(|_| {
            tr!(
                "Не удалось зашифровать набор.",
                "Could not encrypt the bundle."
            )
        })?;
    let mut result = header.to_vec();
    result.extend(ciphertext);
    Ok(result)
}

pub fn decrypt(bytes: &[u8], password: &str) -> Result<Vec<PortableProfile>> {
    if bytes.len() > MAX_BYTES || bytes.len() < HEADER + 16 || &bytes[..8] != MAGIC {
        return Err(tr!(
            "Неподдерживаемый формат, версия или размер файла.",
            "Unsupported file format, version or size."
        )
        .into());
    }
    let key = key(password, &bytes[8..24])?;
    let cipher = XChaCha20Poly1305::new((&*key).into());
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(&bytes[24..HEADER]),
                Payload {
                    msg: &bytes[HEADER..],
                    aad: &bytes[..HEADER],
                },
            )
            .map_err(|_| tr!("Неверный мастер-пароль или файл повреждён. Данные не применены.", "The master password is incorrect or the bundle is corrupt. No changes were applied."))?,
    );
    let bundle: Bundle = serde_json::from_slice(&plaintext).map_err(|_| {
        tr!(
            "Некорректное содержимое набора.",
            "Invalid bundle contents."
        )
    })?;
    if bundle.version != 1 {
        return Err(tr!(
            "Версия набора не поддерживается.",
            "Unsupported bundle version."
        )
        .into());
    }
    validate(&bundle.profiles)?;
    Ok(bundle.profiles)
}

/// Publish immutable exports: no shared file is overwritten by concurrent devices.
pub fn publish(
    directory: &Path,
    profiles: Vec<PortableProfile>,
    password: &str,
) -> Result<PathBuf> {
    let data = encrypt(profiles, password)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(|_| {
        tr!(
            "Не удалось создать файл в выбранной папке.",
            "Could not create a file in the selected folder."
        )
    })?;
    temporary
        .write_all(&data)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|_| {
            tr!(
                "Не удалось записать зашифрованный файл.",
                "Could not write the encrypted file."
            )
        })?;
    let path = directory.join(format!(
        "rdp-profiles-{}.rdpm",
        gtk::glib::uuid_string_random()
    ));
    temporary.persist_noclobber(&path).map_err(|_| {
        tr!(
            "Не удалось опубликовать набор. Существующие файлы не заменены.",
            "Could not publish the bundle. Existing files were retained."
        )
    })?;
    Ok(path)
}

pub fn read(path: &Path, password: &str) -> Result<Vec<PortableProfile>> {
    let file = File::open(path).map_err(|_| {
        tr!(
            "Не удалось открыть выбранный файл.",
            "Could not open the selected file."
        )
    })?;
    if !file
        .metadata()
        .map_err(|_| tr!("Не удалось проверить файл.", "Could not inspect the file."))?
        .is_file()
    {
        return Err(tr!(
            "Выберите обычный файл набора.",
            "Select a regular bundle file."
        )
        .into());
    }
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| tr!("Не удалось прочитать набор.", "Could not read the bundle."))?;
    decrypt(&bytes, password)
}

#[derive(Clone, Copy)]
pub enum ImportAction {
    Add,
    Replace,
    Copy,
}

pub struct Change {
    pub incoming: PortableProfile,
    pub expected: Option<Profile>,
    pub expected_folder: Option<String>,
    pub action: ImportAction,
}

#[cfg(test)]
mod tests {
    use super::*;
    const PASSWORD: &str = "test-master-password";

    fn profiles() -> Vec<PortableProfile> {
        vec![PortableProfile::from(&Profile {
            name: "Работа".into(),
            host: "server.example.test".into(),
            ..Profile::default()
        })]
    }

    #[test]
    fn encrypted_roundtrip_authentication_and_randomness() {
        let mut original = profiles();
        original[0].folder = Some("Работа".into());
        let bytes = encrypt(original.clone(), PASSWORD).unwrap();
        assert_eq!(decrypt(&bytes, PASSWORD).unwrap(), original);
        assert_ne!(encrypt(original, PASSWORD).unwrap(), bytes);
        assert!(!bytes
            .windows(b"server.example.test".len())
            .any(|w| w == b"server.example.test"));
        assert!(decrypt(&bytes, "wrong-password").is_err());
        for position in [8, 24, HEADER, bytes.len() - 1] {
            let mut changed = bytes.clone();
            changed[position] ^= 1;
            assert!(decrypt(&changed, PASSWORD).is_err());
        }
        assert!(decrypt(&bytes[..HEADER], PASSWORD).is_err());
        assert!(decrypt(&vec![0; MAX_BYTES + 1], PASSWORD).is_err());
        assert!(encrypt(profiles(), "short").is_err());
    }

    #[test]
    fn portable_schema_has_only_allowlisted_fields() {
        let json = serde_json::to_value(&profiles()[0]).unwrap();
        let fields: std::collections::BTreeSet<_> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            ["uid", "name", "host", "port", "username", "domain"]
                .into_iter()
                .collect()
        );
        let mut injected = json;
        injected["password"] = "must-not-import".into();
        assert!(serde_json::from_value::<PortableProfile>(injected).is_err());
        let p = profiles();
        assert!(encrypt(vec![p[0].clone(), p[0].clone()], PASSWORD).is_err());
    }

    #[test]
    fn folder_publication_does_not_replace_existing_exports() {
        let directory = tempfile::tempdir().unwrap();
        let first = publish(directory.path(), profiles(), PASSWORD).unwrap();
        let original = std::fs::read(&first).unwrap();
        let second = publish(directory.path(), profiles(), PASSWORD).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), original);
        assert_eq!(read(&first, PASSWORD).unwrap().len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(first).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
