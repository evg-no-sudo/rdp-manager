mod import_ui;
mod profiles_ui;
mod remmina;
mod secrets;
mod store;
mod transfer;
mod transfer_ui;
mod ui;
mod webdav;
mod webdav_ui;

use adw::prelude::*;

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder()
        .application_id("io.github.rdpmanager.RdpManager")
        .build();
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
