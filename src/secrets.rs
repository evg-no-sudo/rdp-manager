//! Device-local secrets. This module is deliberately independent of SQLite and sync data.
use std::collections::HashMap;

fn schema() -> libsecret::Schema {
    libsecret::Schema::new(
        "io.github.rdpmanager.Credential",
        libsecret::SchemaFlags::NONE,
        HashMap::from([
            ("kind", libsecret::SchemaAttributeType::String),
            ("uid", libsecret::SchemaAttributeType::String),
        ]),
    )
}

fn attributes(uid: &str) -> HashMap<&str, &str> {
    HashMap::from([("kind", "rdp-password"), ("uid", uid)])
}

pub const UNAVAILABLE: &str = "Не удалось обратиться к Secret Service. Проверьте, что системное хранилище паролей запущено и разблокировано. Пароль не записан в файл или SQLite.";

fn explain(error: gtk::glib::Error) -> &'static str {
    use gtk::gio::{DBusError, IOErrorEnum};
    if error.matches(DBusError::ServiceUnknown) || error.matches(DBusError::NameHasNoOwner) {
        "Secret Service недоступен: в пользовательской сессии нет службы хранения паролей."
    } else if error.matches(libsecret::Error::IsLocked) {
        "Системное хранилище заблокировано. Разблокируйте его и повторите сохранение."
    } else if error.matches(IOErrorEnum::Cancelled) {
        "Операция с хранилищем отменена. Повторите её и подтвердите системный запрос."
    } else if error.matches(DBusError::AccessDenied) || error.matches(IOErrorEnum::PermissionDenied)
    {
        "Secret Service отказал в доступе. Проверьте разрешения системного хранилища."
    } else if error.matches(libsecret::Error::NoSuchObject) {
        "Коллекция паролей недоступна. Проверьте наличие хранилища по умолчанию в менеджере паролей."
    } else {
        UNAVAILABLE
    }
}

pub async fn store(uid: &str, password: &str) -> Result<(), &'static str> {
    libsecret::password_store_future(
        Some(&schema()),
        attributes(uid),
        Some(libsecret::COLLECTION_DEFAULT),
        "Linux RDP Manager — пароль подключения",
        password,
    )
    .await
    .map_err(explain)?;
    // Some providers/prompts can complete without persisting the item. Do not
    // close the editor as successful until the stored value can be read back.
    let stored = libsecret::password_lookup_future(Some(&schema()), attributes(uid))
        .await
        .map_err(explain)?;
    if stored.as_deref() != Some(password) {
        return Err("Хранилище не подтвердило сохранение пароля. Проверьте разблокировку и повторите попытку.");
    }
    Ok(())
}

pub async fn exists(uid: &str) -> Result<bool, &'static str> {
    // Do not put a retrieved password in a widget, status message or diagnostic output.
    libsecret::password_lookup_future(Some(&schema()), attributes(uid))
        .await
        .map(|value| value.is_some())
        .map_err(explain)
}

pub async fn clear(uid: &str) -> Result<(), &'static str> {
    libsecret::password_clear_future(Some(&schema()), attributes(uid))
        .await
        .map_err(explain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_attributes_contain_only_kind_and_opaque_identity() {
        let attributes = attributes("test-profile-uuid");
        assert_eq!(attributes.len(), 2);
        assert_eq!(attributes["kind"], "rdp-password");
        assert_eq!(attributes["uid"], "test-profile-uuid");
    }

    #[test]
    #[ignore = "Requires an isolated, unlocked Secret Service session; see docs/TESTING.md"]
    fn isolated_secret_service_lifecycle() {
        assert_eq!(
            std::env::var("RDP_MANAGER_SECRET_TEST").as_deref(),
            Ok("isolated")
        );
        gtk::glib::MainContext::new().block_on(async {
            let uid = gtk::glib::uuid_string_random();
            assert!(!exists(&uid).await.unwrap());
            store(&uid, "synthetic-test-value").await.unwrap();
            assert!(exists(&uid).await.unwrap());
            store(&uid, "replacement-test-value").await.unwrap();
            let value = libsecret::password_lookup_future(Some(&schema()), attributes(&uid))
                .await
                .unwrap()
                .unwrap();
            assert!(value.as_str() == "replacement-test-value");
            clear(&uid).await.unwrap();
            assert!(!exists(&uid).await.unwrap());
        });
    }
}
