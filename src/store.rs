use rusqlite::{params, Connection};
use std::{error::Error, fs, path::Path};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub id: Option<i64>,
    pub uid: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: String,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            id: None,
            uid: gtk::glib::uuid_string_random().into(),
            name: String::new(),
            host: String::new(),
            port: 3389,
            username: String::new(),
            domain: String::new(),
        }
    }
}

impl Profile {
    pub fn validate(&self) -> Result<()> {
        if !gtk::glib::uuid_string_is_valid(&self.uid) {
            return Err("Некорректный идентификатор профиля.".into());
        }
        if self.name.trim().is_empty() || self.host.trim().is_empty() {
            return Err("Заполните название и сервер.".into());
        }
        if self.port == 0 {
            return Err("Порт должен быть от 1 до 65535.".into());
        }
        if self.host.chars().any(char::is_whitespace)
            || self.host.contains('/')
            || self.host.contains('@')
        {
            return Err("Введите имя сервера или IP без схемы, пользователя и пробелов. Порт указывается отдельно.".into());
        }
        if [&self.name, &self.host, &self.username, &self.domain]
            .iter()
            .any(|s| s.chars().any(char::is_control))
        {
            return Err("Поля не должны содержать управляющие символы.".into());
        }
        Ok(())
    }
}

pub struct Store(Connection);

impl Store {
    pub fn open_default() -> Result<Self> {
        let directory = gtk::glib::user_data_dir().join("rdp-manager");
        fs::create_dir_all(&directory)?;
        Self::open(&directory.join("profiles.sqlite3"))
    }

    pub fn open(path: &Path) -> Result<Self> {
        Self::initialize(Connection::open(path)?)
    }

    fn initialize(mut connection: Connection) -> Result<Self> {
        connection.busy_timeout(std::time::Duration::from_secs(3))?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 3 {
            return Err("База создана более новой версией приложения.".into());
        }
        if version == 0 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(
                "CREATE TABLE profiles (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
                    host TEXT NOT NULL CHECK(length(trim(host)) > 0),
                    port INTEGER NOT NULL CHECK(port BETWEEN 1 AND 65535),
                    username TEXT NOT NULL DEFAULT '',
                    domain TEXT NOT NULL DEFAULT ''
                );
                PRAGMA user_version = 1;",
            )?;
            transaction.commit()?;
        }
        if version < 2 {
            let transaction = connection.transaction()?;
            transaction.execute_batch("ALTER TABLE profiles ADD COLUMN uid TEXT;")?;
            let ids = {
                let mut query = transaction.prepare("SELECT id FROM profiles")?;
                let rows = query.query_map([], |row| row.get::<_, i64>(0))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            for id in ids {
                transaction.execute(
                    "UPDATE profiles SET uid=?1 WHERE id=?2",
                    params![gtk::glib::uuid_string_random().as_str(), id],
                )?;
            }
            transaction.execute_batch("CREATE UNIQUE INDEX profiles_uid ON profiles(uid);
                CREATE TRIGGER profiles_uid_required_insert BEFORE INSERT ON profiles
                WHEN NEW.uid IS NULL OR length(NEW.uid)=0 BEGIN SELECT RAISE(ABORT, 'profile uid required'); END;
                CREATE TRIGGER profiles_uid_immutable BEFORE UPDATE OF uid ON profiles
                WHEN NEW.uid IS NOT OLD.uid BEGIN SELECT RAISE(ABORT, 'profile uid immutable'); END;
                PRAGMA user_version = 2;")?;
            transaction.commit()?;
        }
        if version < 3 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(
                "CREATE TABLE trusted_sftp_hosts (
                host TEXT NOT NULL, port INTEGER NOT NULL CHECK(port BETWEEN 1 AND 65535),
                public_key BLOB NOT NULL, PRIMARY KEY(host,port)
            ); PRAGMA user_version=3;",
            )?;
            transaction.commit()?;
        }
        Ok(Self(connection))
    }

    pub fn sftp_host_key(&self, host: &str, port: u16) -> Result<Option<Vec<u8>>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .0
            .query_row(
                "SELECT public_key FROM trusted_sftp_hosts WHERE host=?1 AND port=?2",
                params![host, port],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn trust_sftp_host(&self, host: &str, port: u16, key: &[u8]) -> Result<()> {
        let transaction = self.0.unchecked_transaction()?;
        transaction.execute(
            "INSERT OR IGNORE INTO trusted_sftp_hosts(host,port,public_key) VALUES(?1,?2,?3)",
            params![host, port, key],
        )?;
        if self.sftp_host_key(host, port)?.as_deref() != Some(key) {
            return Err("Ключ сервера уже изменился. Существующее доверие не заменено.".into());
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn profiles(&self) -> Result<Vec<Profile>> {
        let mut query = self.0.prepare(
            "SELECT id, name, host, port, username, domain, uid FROM profiles ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = query.query_map([], |row| {
            Ok(Profile {
                id: Some(row.get(0)?),
                uid: row.get(6)?,
                name: row.get(1)?,
                host: row.get(2)?,
                port: row.get(3)?,
                username: row.get(4)?,
                domain: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn save(&self, profile: &Profile) -> Result<i64> {
        profile.validate()?;
        let values = params![
            profile.name,
            profile.host,
            profile.port,
            profile.username,
            profile.domain,
            profile.uid
        ];
        if let Some(id) = profile.id {
            let count = self.0.execute(
                "UPDATE profiles SET name=?1, host=?2, port=?3, username=?4, domain=?5 WHERE id=?6 AND uid=?7",
                params![
                    profile.name,
                    profile.host,
                    profile.port,
                    profile.username,
                    profile.domain,
                    id,
                    profile.uid
                ],
            )?;
            if count == 0 {
                return Err("Профиль уже удалён. Закройте форму и обновите список.".into());
            }
            Ok(id)
        } else {
            self.0.execute(
                "INSERT INTO profiles(name,host,port,username,domain,uid) VALUES(?1,?2,?3,?4,?5,?6)",
                values,
            )?;
            Ok(self.0.last_insert_rowid())
        }
    }

    /// Atomically add profiles, skipping exact identities already in the database/batch.
    pub fn import(&self, profiles: &[Profile]) -> Result<usize> {
        for profile in profiles {
            profile.validate()?;
        }
        let transaction = self.0.unchecked_transaction()?;
        let mut added = 0;
        for p in profiles {
            added += transaction.execute(
                "INSERT INTO profiles(name,host,port,username,domain,uid)
                 SELECT ?1,?2,?3,?4,?5,?6 WHERE NOT EXISTS (
                     SELECT 1 FROM profiles WHERE name=?1 AND host=?2 COLLATE NOCASE
                     AND port=?3 AND username=?4 AND domain=?5
                 )",
                params![p.name, p.host, p.port, p.username, p.domain, p.uid],
            )?;
        }
        transaction.commit()?;
        Ok(added)
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.0.execute("DELETE FROM profiles WHERE id=?1", [id])?;
        Ok(())
    }

    pub fn apply_transfer(
        &self,
        changes: &[crate::transfer::Change],
    ) -> Result<(usize, usize, usize)> {
        use crate::transfer::ImportAction;
        let transaction = self.0.unchecked_transaction()?;
        let local = self.profiles()?;
        let mut ids = std::collections::HashSet::new();
        let mut additions: Vec<&crate::transfer::PortableProfile> = Vec::new();
        for change in changes {
            change.incoming.profile().validate()?;
            if !ids.insert(&change.incoming.uid) {
                return Err("Повторяющийся идентификатор профиля.".into());
            }
            let current = local.iter().find(|p| p.uid == change.incoming.uid);
            if current != change.expected.as_ref() {
                return Err(
                    "Профили изменились после просмотра. Откройте файл заново и повторите выбор."
                        .into(),
                );
            }
            match change.action {
                ImportAction::Add if current.is_none() => {
                    if local.iter().any(|p| change.incoming.same_fields(p)) {
                        return Err(
                            "Один из профилей уже появился в базе. Откройте файл заново.".into(),
                        );
                    }
                    if additions
                        .iter()
                        .any(|p| change.incoming.same_fields(&p.profile()))
                    {
                        return Err("В выбранном наборе повторяется одно подключение. Оставьте одну запись.".into());
                    }
                    additions.push(&change.incoming);
                }
                ImportAction::Replace | ImportAction::Copy if current.is_some() => {}
                _ => return Err("Некорректное действие переноса.".into()),
            }
        }
        let (mut added, mut replaced, mut copied) = (0, 0, 0);
        for change in changes {
            let mut p = change.incoming.profile();
            match change.action {
                ImportAction::Add => {
                    added += 1;
                }
                ImportAction::Replace => {
                    p.id = change.expected.as_ref().and_then(|p| p.id);
                    replaced += 1;
                }
                ImportAction::Copy => {
                    p.uid = gtk::glib::uuid_string_random().into();
                    p.name.push_str(" (копия)");
                    copied += 1;
                }
            }
            self.save(&p)?;
        }
        transaction.commit()?;
        Ok((added, replaced, copied))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_lifecycle_persists_and_rejects_invalid_data() -> Result<()> {
        let path =
            std::env::temp_dir().join(format!("rdp-manager-test-{}.sqlite3", std::process::id()));
        let store = Store::open(&path)?;
        let mut profile = Profile {
            name: "Office ' SQL".into(),
            host: "2001:db8::1".into(),
            ..Profile::default()
        };
        store.save(&profile)?;
        profile = store.profiles()?.remove(0);
        profile.port = 3390;
        store.save(&profile)?;
        drop(store);
        let store = Store::open(&path)?;
        assert_eq!(store.profiles()?, vec![profile.clone()]);
        profile.port = 0;
        assert!(store.save(&profile).is_err());
        profile.host = "rdp://server".into();
        profile.port = 3389;
        assert!(store.save(&profile).is_err());
        store.delete(profile.id.unwrap())?;
        assert!(store.profiles()?.is_empty());
        profile.host = "server.example.test".into();
        assert!(store.save(&profile).is_err());
        drop(store);
        fs::remove_file(path)?;
        Ok(())
    }

    #[test]
    fn future_schema_is_not_modified() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.pragma_update(None, "user_version", 99)?;
        assert!(Store::initialize(connection).is_err());
        Ok(())
    }

    #[test]
    fn sftp_trust_is_device_local_port_specific_and_cannot_be_silently_replaced() -> Result<()> {
        let store = Store::initialize(Connection::open_in_memory()?)?;
        assert!(store.sftp_host_key("server.test", 22)?.is_none());
        store.trust_sftp_host("server.test", 22, b"public-key-one")?;
        store.trust_sftp_host("server.test", 22, b"public-key-one")?;
        assert!(store
            .trust_sftp_host("server.test", 22, b"public-key-two")
            .is_err());
        assert_eq!(
            store.sftp_host_key("server.test", 22)?.as_deref(),
            Some(b"public-key-one".as_slice())
        );
        assert!(store.sftp_host_key("server.test", 2222)?.is_none());
        assert!(store.profiles()?.is_empty());
        Ok(())
    }

    #[test]
    fn migrates_existing_profiles_to_stable_unique_identities() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch("CREATE TABLE profiles(id INTEGER PRIMARY KEY, name TEXT NOT NULL, host TEXT NOT NULL, port INTEGER NOT NULL, username TEXT NOT NULL, domain TEXT NOT NULL);
            INSERT INTO profiles VALUES(1,'Office','server.test',3389,'alice','EXAMPLE');
            INSERT INTO profiles VALUES(2,'Other','other.test',3389,'bob','');
            PRAGMA user_version=1;")?;
        let store = Store::initialize(connection)?;
        let mut profiles = store.profiles()?;
        assert_eq!(profiles.len(), 2);
        assert_ne!(profiles[0].uid, profiles[1].uid);
        assert!(profiles
            .iter()
            .all(|p| gtk::glib::uuid_string_is_valid(&p.uid)));
        let uid = profiles[0].uid.clone();
        profiles[0].name = "Renamed".into();
        store.save(&profiles[0])?;
        let p = store
            .profiles()?
            .into_iter()
            .find(|p| p.uid == uid)
            .unwrap();
        assert_eq!(p.name, "Renamed");
        assert_eq!(p.username, "alice");
        assert_eq!(p.domain, "EXAMPLE");
        let mut invalid = p;
        invalid.uid = gtk::glib::uuid_string_random().into();
        assert!(store.save(&invalid).is_err());
        Ok(())
    }

    #[test]
    fn import_is_atomic_and_skips_duplicates() -> Result<()> {
        let store = Store::initialize(Connection::open_in_memory()?)?;
        let p = Profile {
            name: "Imported".into(),
            host: "server.test".into(),
            ..Profile::default()
        };
        assert_eq!(store.import(&[p.clone(), p.clone()])?, 1);
        assert_eq!(store.import(std::slice::from_ref(&p))?, 0);
        let mut other = p.clone();
        other.host = "other.test".into();
        other.uid = gtk::glib::uuid_string_random().into();
        let mut invalid = p.clone();
        invalid.port = 0;
        assert!(store.import(&[other.clone(), invalid]).is_err());
        assert_eq!(store.profiles()?.len(), 1);
        // A database error midway through the batch must roll back earlier inserts.
        store.0.execute_batch("CREATE TRIGGER reject_bad BEFORE INSERT ON profiles WHEN NEW.name='bad' BEGIN SELECT RAISE(ABORT, 'test rejection'); END;")?;
        let mut rejected = p;
        rejected.name = "bad".into();
        rejected.uid = gtk::glib::uuid_string_random().into();
        assert!(store.import(&[other, rejected]).is_err());
        assert_eq!(store.profiles()?.len(), 1);
        Ok(())
    }

    #[test]
    fn transfer_preserves_identity_detects_stale_preview_and_copies_conflicts() -> Result<()> {
        use crate::transfer::{Change, ImportAction, PortableProfile};
        let store = Store::initialize(Connection::open_in_memory()?)?;
        let p = Profile {
            name: "Office".into(),
            host: "office.test".into(),
            ..Profile::default()
        };
        let incoming = PortableProfile::from(&p);
        assert_eq!(
            store.apply_transfer(&[Change {
                incoming: incoming.clone(),
                expected: None,
                action: ImportAction::Add
            }])?,
            (1, 0, 0)
        );
        assert_eq!(store.profiles()?[0].uid, p.uid);
        let expected = store.profiles()?.remove(0);
        let mut remote = incoming;
        remote.host = "remote.test".into();
        let mut local = expected.clone();
        local.name = "Changed during preview".into();
        store.save(&local)?;
        let change = Change {
            incoming: remote.clone(),
            expected: Some(expected),
            action: ImportAction::Replace,
        };
        assert!(store.apply_transfer(&[change]).is_err());
        assert_eq!(store.profiles()?[0].host, "office.test");
        store.apply_transfer(&[Change {
            incoming: remote.clone(),
            expected: Some(local.clone()),
            action: ImportAction::Replace,
        }])?;
        assert_eq!(store.profiles()?[0].uid, p.uid);
        assert_eq!(store.profiles()?[0].id, local.id);
        let expected = store.profiles()?.remove(0);
        store.apply_transfer(&[Change {
            incoming: remote,
            expected: Some(expected),
            action: ImportAction::Copy,
        }])?;
        let profiles = store.profiles()?;
        assert_eq!(profiles.len(), 2);
        assert_ne!(profiles[0].uid, profiles[1].uid);
        Ok(())
    }

    #[test]
    fn transfer_rolls_back_on_mid_batch_database_error() -> Result<()> {
        use crate::transfer::{Change, ImportAction, PortableProfile};
        let store = Store::initialize(Connection::open_in_memory()?)?;
        store.0.execute_batch("CREATE TRIGGER reject_bad_transfer BEFORE INSERT ON profiles WHEN NEW.name='bad' BEGIN SELECT RAISE(ABORT, 'test rejection'); END;")?;
        let changes: Vec<_> = ["good", "bad"]
            .iter()
            .map(|name| Change {
                incoming: PortableProfile::from(&Profile {
                    name: (*name).into(),
                    host: "server.test".into(),
                    ..Profile::default()
                }),
                expected: None,
                action: ImportAction::Add,
            })
            .collect();
        assert!(store.apply_transfer(&changes).is_err());
        assert!(store.profiles()?.is_empty());
        Ok(())
    }
}
