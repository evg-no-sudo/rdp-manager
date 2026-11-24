#[macro_use]
mod i18n;
mod clipboard_files;
mod import_ui;
mod profiles_ui;
mod rdp;
mod rdp_input;
mod rdp_ui;
mod remmina;
mod secrets;
mod sftp;
mod sftp_ui;
mod store;
mod transfer;
mod transfer_ui;
mod ui;
mod webdav;
mod webdav_ui;

use adw::prelude::*;

const APP_ID: &str = "io.github.rdpmanager.RdpManager";

fn theme_scheme(theme: &str) -> Option<adw::ColorScheme> {
    match theme {
        "system" => Some(adw::ColorScheme::Default),
        "light" => Some(adw::ColorScheme::ForceLight),
        "dark" => Some(adw::ColorScheme::ForceDark),
        _ => None,
    }
}

fn install_theme_action(app: &adw::Application) {
    use gtk::{gio, glib};
    use std::io::Write;

    let config = glib::user_config_dir().join("rdp-manager");
    let path = config.join("theme");
    let saved = std::fs::read_to_string(&path).unwrap_or_default();
    let (theme, scheme) = theme_scheme(saved.trim())
        .map(|scheme| (saved.trim(), scheme))
        .unwrap_or(("system", adw::ColorScheme::Default));
    adw::StyleManager::default().set_color_scheme(scheme);
    let action = gio::SimpleAction::new_stateful(
        "theme",
        Some(glib::VariantTy::STRING),
        &theme.to_variant(),
    );
    action.connect_change_state(move |action, value| {
        let Some(theme) = value.and_then(glib::Variant::str) else {
            return;
        };
        let Some(scheme) = theme_scheme(theme) else {
            return;
        };
        adw::StyleManager::default().set_color_scheme(scheme);
        action.set_state(&theme.to_variant());
        let save = || -> std::io::Result<()> {
            std::fs::create_dir_all(&config)?;
            let mut file = tempfile::NamedTempFile::new_in(&config)?;
            writeln!(file, "{theme}")?;
            file.persist(&path)?;
            Ok(())
        };
        if let Err(error) = save() {
            eprintln!(
                "{}",
                tr_format!(
                    "Не удалось сохранить тему: {error}",
                    "Could not save theme: {error}"
                )
            );
        }
    });
    app.add_action(&action);
}

fn main() -> gtk::glib::ExitCode {
    if std::env::args().skip(1).any(|arg| arg == "--diagnostics") {
        println!("{}", rdp::diagnostics());
        return gtk::glib::ExitCode::SUCCESS;
    }
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|app| {
        install_theme_action(app);
        i18n::install_action(app);
        if let (Some(display), Some(path)) = (
            gtk::gdk::Display::default(),
            std::env::var_os("RDP_MANAGER_ICON_PATH"),
        ) {
            gtk::IconTheme::for_display(&display).add_search_path(std::path::PathBuf::from(path));
        }
        gtk::Window::set_default_icon_name(APP_ID);
    });
    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
        } else {
            match store::Store::open_default() {
                Ok(store) => ui::create_window(app, ui::State::new(store)).0.present(),
                Err(error) => {
                    let window = adw::ApplicationWindow::builder()
                        .application(app)
                        .title("Linux RDP Manager")
                        .default_width(600)
                        .default_height(300)
                        .build();
                    let status = adw::StatusPage::builder()
                        .title(tr!(
                            "Не удалось открыть базу данных",
                            "Could not open the database"
                        ))
                        .description(error.to_string())
                        .build();
                    window.set_content(Some(&status));
                    window.present();
                }
            }
        }
    });
    app.run()
}
