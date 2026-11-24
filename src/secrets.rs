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

fn explain(error: gtk::glib::Error) -> &'static str {
    use gtk::gio::{DBusError, IOErrorEnum};
    if error.matches(DBusError::ServiceUnknown) || error.matches(DBusError::NameHasNoOwner) {
        tr!(
            "Secret Service недоступен: в пользовательской сессии нет службы хранения паролей.",
            "Secret Service is unavailable in this user session."
        )
    } else if error.matches(libsecret::Error::IsLocked) {
        tr!(
            "Системное хранилище заблокировано. Разблокируйте его и повторите сохранение.",
            "Unlock the system keyring and try saving again."
        )
    } else if error.matches(IOErrorEnum::Cancelled) {
        tr!(
            "Операция с хранилищем отменена. Повторите её и подтвердите системный запрос.",
            "Keyring operation cancelled. Try again and accept the system prompt."
        )
    } else if error.matches(DBusError::AccessDenied) || error.matches(IOErrorEnum::PermissionDenied)
    {
        tr!(
            "Secret Service отказал в доступе. Проверьте разрешения системного хранилища.",
            "Secret Service denied access. Check the keyring permissions."
        )
    } else if error.matches(libsecret::Error::NoSuchObject) {
        tr!("Коллекция паролей недоступна. Проверьте наличие хранилища по умолчанию в менеджере паролей.", "The password collection is unavailable. Check the default keyring in your password manager.")
    } else {
        tr!("Не удалось обратиться к Secret Service. Проверьте, что системное хранилище паролей запущено и разблокировано. Пароль не записан в файл или SQLite.", "Could not reach Secret Service. Check that the system keyring is running and unlocked.")
    }
}

pub async fn store(uid: &str, password: &str) -> Result<(), &'static str> {
    libsecret::password_store_future(
        Some(&schema()),
        attributes(uid),
        Some(libsecret::COLLECTION_DEFAULT),
        tr!(
            "Linux RDP Manager — пароль подключения",
            "Linux RDP Manager — connection password"
        ),
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
        return Err(tr!("Хранилище не подтвердило сохранение пароля. Проверьте разблокировку и повторите попытку.", "The keyring did not confirm saving the password. Unlock it and try again."));
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

pub async fn lookup_rdp(uid: &str) -> Result<Option<zeroize::Zeroizing<String>>, &'static str> {
    libsecret::password_lookup_future(Some(&schema()), attributes(uid))
        .await
        .map(|value| value.map(|value| zeroize::Zeroizing::new(value.to_string())))
        .map_err(explain)
}

pub async fn clear(uid: &str) -> Result<(), &'static str> {
    libsecret::password_clear_future(Some(&schema()), attributes(uid))
        .await
        .map_err(explain)
}

fn webdav_attributes(uid: &str) -> HashMap<&str, &str> {
    HashMap::from([("kind", "webdav-password"), ("uid", uid)])
}

pub async fn store_webdav(uid: &str, password: &str) -> Result<(), &'static str> {
    libsecret::password_store_future(
        Some(&schema()),
        webdav_attributes(uid),
        Some(libsecret::COLLECTION_DEFAULT),
        tr!(
            "Linux RDP Manager — пароль приложения WebDAV",
            "Linux RDP Manager — WebDAV app password"
        ),
        password,
    )
    .await
    .map_err(explain)?;
    if lookup_webdav(uid).await?.as_deref().map(String::as_str) != Some(password) {
        return Err(tr!(
            "Хранилище не подтвердило сохранение пароля WebDAV.",
            "The keyring did not confirm saving the WebDAV password."
        ));
    }
    Ok(())
}

pub async fn lookup_webdav(uid: &str) -> Result<Option<zeroize::Zeroizing<String>>, &'static str> {
    libsecret::password_lookup_future(Some(&schema()), webdav_attributes(uid))
        .await
        .map(|value| value.map(|value| zeroize::Zeroizing::new(value.to_string())))
        .map_err(explain)
}

pub async fn clear_webdav(uid: &str) -> Result<(), &'static str> {
    libsecret::password_clear_future(Some(&schema()), webdav_attributes(uid))
        .await
        .map_err(explain)
}

fn sftp_attributes(uid: &str) -> HashMap<&str, &str> {
    HashMap::from([("kind", "sftp-auth-secret"), ("uid", uid)])
}

pub async fn store_sftp(uid: &str, secret: &str) -> Result<(), &'static str> {
    libsecret::password_store_future(
        Some(&schema()),
        sftp_attributes(uid),
        Some(libsecret::COLLECTION_DEFAULT),
        tr!(
            "Linux RDP Manager — пароль/passphrase SFTP",
            "Linux RDP Manager — SFTP password or passphrase"
        ),
        secret,
    )
    .await
    .map_err(explain)?;
    if lookup_sftp(uid).await?.as_deref().map(String::as_str) != Some(secret) {
        return Err(tr!(
            "Хранилище не подтвердило сохранение секрета SFTP.",
            "The keyring did not confirm saving the SFTP credential."
        ));
    }
    Ok(())
}

pub async fn lookup_sftp(uid: &str) -> Result<Option<zeroize::Zeroizing<String>>, &'static str> {
    libsecret::password_lookup_future(Some(&schema()), sftp_attributes(uid))
        .await
        .map(|value| value.map(|value| zeroize::Zeroizing::new(value.to_string())))
        .map_err(explain)
}

pub async fn clear_sftp(uid: &str) -> Result<(), &'static str> {
    libsecret::password_clear_future(Some(&schema()), sftp_attributes(uid))
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
            assert!(lookup_rdp(&uid).await.unwrap().unwrap().as_str() == "synthetic-test-value");
            assert!(exists(&uid).await.unwrap());
            store(&uid, "replacement-test-value").await.unwrap();
            let value = libsecret::password_lookup_future(Some(&schema()), attributes(&uid))
                .await
                .unwrap()
                .unwrap();
            assert!(value.as_str() == "replacement-test-value");
            clear(&uid).await.unwrap();
            assert!(!exists(&uid).await.unwrap());
            store(&uid, "rdp-test-value").await.unwrap();
            store_webdav(&uid, "webdav-test-token").await.unwrap();
            let token = lookup_webdav(&uid).await.unwrap().unwrap();
            assert!(token.as_str() == "webdav-test-token");
            store_sftp(&uid, "sftp-test-passphrase").await.unwrap();
            assert!(lookup_sftp(&uid).await.unwrap().unwrap().as_str() == "sftp-test-passphrase");
            clear_sftp(&uid).await.unwrap();
            assert!(lookup_sftp(&uid).await.unwrap().is_none());
            assert!(lookup_webdav(&uid).await.unwrap().is_some());
            clear_webdav(&uid).await.unwrap();
            assert!(lookup_webdav(&uid).await.unwrap().is_none());
            assert!(exists(&uid).await.unwrap());
            clear(&uid).await.unwrap();
        });
    }
}
