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

fn main() -> gtk::glib::ExitCode {
    if std::env::args().skip(1).any(|arg| arg == "--diagnostics") {
        println!("{}", rdp::diagnostics());
        return gtk::glib::ExitCode::SUCCESS;
    }
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|_| {
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
                        .title("Не удалось открыть базу данных")
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
