use rusqlite::{params, Connection};
use std::{error::Error, fs, path::Path};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub id: Option<i64>,
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
        if version > 1 {
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
        Ok(Self(connection))
    }

    pub fn profiles(&self) -> Result<Vec<Profile>> {
        let mut query = self.0.prepare(
            "SELECT id, name, host, port, username, domain FROM profiles ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = query.query_map([], |row| {
            Ok(Profile {
                id: Some(row.get(0)?),
                name: row.get(1)?,
                host: row.get(2)?,
                port: row.get(3)?,
                username: row.get(4)?,
                domain: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn save(&self, profile: &Profile) -> Result<()> {
        profile.validate()?;
        let values = params![
            profile.name,
            profile.host,
            profile.port,
            profile.username,
            profile.domain
        ];
        if let Some(id) = profile.id {
            let count = self.0.execute(
                "UPDATE profiles SET name=?1, host=?2, port=?3, username=?4, domain=?5 WHERE id=?6",
                params![
                    profile.name,
                    profile.host,
                    profile.port,
                    profile.username,
                    profile.domain,
                    id
                ],
            )?;
            if count == 0 {
                return Err("Профиль уже удалён. Закройте форму и обновите список.".into());
            }
        } else {
            self.0.execute(
                "INSERT INTO profiles(name,host,port,username,domain) VALUES(?1,?2,?3,?4,?5)",
                values,
            )?;
        }
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.0.execute("DELETE FROM profiles WHERE id=?1", [id])?;
        Ok(())
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
}
