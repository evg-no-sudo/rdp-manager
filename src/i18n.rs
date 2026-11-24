//! The interface language is fixed for one application run.
use adw::prelude::*;
use gtk::{gio, glib};
use std::{io::Write, path::PathBuf, sync::OnceLock};

static RUSSIAN: OnceLock<bool> = OnceLock::new();

fn preference_path() -> PathBuf {
    glib::user_config_dir().join("rdp-manager/language")
}

pub fn russian() -> bool {
    *RUSSIAN.get_or_init(|| {
        match std::fs::read_to_string(preference_path())
            .unwrap_or_default()
            .trim()
        {
            "ru" => true,
            "en" => false,
            _ => glib::language_names()
                .first()
                .is_some_and(|locale| locale.split(['_', '-', '.', '@']).next() == Some("ru")),
        }
    })
}

macro_rules! tr {
    ($ru:literal, $en:literal) => {
        if $crate::i18n::russian() {
            $ru
        } else {
            $en
        }
    };
}

macro_rules! tr_format {
    ($ru:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::russian() { format!($ru $(, $arg)*) }
        else { format!($en $(, $arg)*) }
    };
}

pub fn install_action(app: &adw::Application) {
    let path = preference_path();
    let saved = std::fs::read_to_string(&path).unwrap_or_default();
    let initial = match saved.trim() {
        "ru" => "ru",
        "en" => "en",
        _ => "system",
    };
    let action = gio::SimpleAction::new_stateful(
        "language",
        Some(glib::VariantTy::STRING),
        &initial.to_variant(),
    );
    let weak_app = app.downgrade();
    action.connect_change_state(move |action, value| {
        let Some(value) = value
            .and_then(glib::Variant::str)
            .filter(|v| matches!(*v, "system" | "ru" | "en"))
        else {
            return;
        };
        if action.state().as_ref().and_then(glib::Variant::str) == Some(value) {
            return;
        }
        let saved = (|| -> std::io::Result<()> {
            let dir = path
                .parent()
                .expect("language has a configuration directory");
            std::fs::create_dir_all(dir)?;
            let mut file = tempfile::NamedTempFile::new_in(dir)?;
            writeln!(file, "{value}")?;
            file.persist(&path)?;
            Ok(())
        })();
        let dialog = match saved {
            Ok(()) => {
                action.set_state(&value.to_variant());
                adw::AlertDialog::builder()
                    .heading(tr!("Язык сохранён", "Language saved"))
                    .body(tr!(
                        "Перезапустите приложение, чтобы сменить язык.",
                        "Restart the application to change the language."
                    ))
                    .build()
            }
            Err(error) => adw::AlertDialog::builder()
                .heading(tr!("Не удалось сохранить язык", "Could not save language"))
                .body(error.to_string())
                .build(),
        };
        dialog.add_response("close", tr!("Закрыть", "Close"));
        if let Some(window) = weak_app.upgrade().and_then(|app| app.active_window()) {
            dialog.present(Some(&window));
        }
    });
    app.add_action(&action);
}
