use crate::{
    secrets,
    transfer::{self, PortableProfile},
    transfer_ui,
    ui::State,
    webdav::{Dav, Settings, Snapshot},
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{cell::RefCell, rc::Rc};
use zeroize::Zeroizing;

struct Form {
    dialog: glib::WeakRef<adw::Dialog>,
    root: glib::WeakRef<gtk::Box>,
    directory: adw::EntryRow,
    username: adw::EntryRow,
    token: adw::PasswordEntryRow,
    master: adw::PasswordEntryRow,
    confirm: adw::PasswordEntryRow,
    status: gtk::Label,
    remote: gtk::DropDown,
    snapshots: RefCell<Vec<Snapshot>>,
    listed_for: RefCell<Option<(String, String)>>,
    saved: RefCell<Option<Settings>>,
    candidates: Vec<(gtk::CheckButton, PortableProfile)>,
    state: Rc<State>,
}

#[derive(Clone, Copy)]
enum Action {
    Save,
    List,
    Upload,
    Download,
}
enum Outcome {
    Listed(Vec<Snapshot>),
    Uploaded(String),
    Downloaded(Vec<PortableProfile>),
}

fn notify(label: &gtk::Label, message: &str, success: bool) {
    label.remove_css_class("success");
    label.remove_css_class("error");
    label.add_css_class(if success { "success" } else { "error" });
    label.set_text(message);
    label.set_visible(true);
}

pub fn show(parent: &adw::ApplicationWindow, state: Rc<State>) {
    let dialog = adw::Dialog::builder()
        .title("WebDAV / Nextcloud")
        .content_width(700)
        .content_height(680)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&adw::HeaderBar::new());
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(8);
    body.set_margin_bottom(12);
    body.set_margin_start(16);
    body.set_margin_end(16);
    let notice = gtk::Label::builder().wrap(true).xalign(0.0)
        .label(tr!("Ручной зашифрованный обмен наборами. Пароли RDP не отправляются. Каждый экспорт создаёт новый набор; автоматического слияния и переноса удалений пока нет. Сначала создайте отдельную папку в Nextcloud и укажите её WebDAV URL.", "Exchange encrypted bundles. Create a separate Nextcloud folder and enter its WebDAV URL. RDP passwords stay on this computer.")).build();
    body.append(&notice);
    let settings = Settings::load();
    let saved = settings.as_ref().ok().and_then(|value| value.clone());
    let group = adw::PreferencesGroup::builder().title(tr!("Хранилище", "Storage"))
        .description(tr!("Пример: https://cloud.example/remote.php/dav/files/USER/RdpManager/ — используйте пароль приложения Nextcloud. При изменении URL или пользователя введите пароль заново.", "Example: https://cloud.example/remote.php/dav/files/USER/RdpManager/. Use a Nextcloud app password and re-enter it if you change the URL or username.")).build();
    let directory = adw::EntryRow::builder()
        .title(tr!("HTTPS URL каталога", "Folder HTTPS URL"))
        .text(saved.as_ref().map(|s| s.directory.as_str()).unwrap_or(""))
        .build();
    let username = adw::EntryRow::builder()
        .title(tr!("Пользователь WebDAV", "WebDAV username"))
        .text(saved.as_ref().map(|s| s.username.as_str()).unwrap_or(""))
        .build();
    let token = adw::PasswordEntryRow::builder()
        .title(tr!(
            "Пароль приложения (пусто — взять сохранённый)",
            "App password (leave blank to use saved password)"
        ))
        .build();
    group.add(&directory);
    group.add(&username);
    group.add(&token);
    body.append(&group);
    let save = gtk::Button::with_label(tr!(
        "Сохранить настройки и пароль приложения на этом ПК",
        "Save settings and app password on this computer"
    ));
    body.append(&save);
    let group = adw::PreferencesGroup::builder().title(tr!("Шифрование набора", "Bundle Encryption"))
        .description(tr!("Мастер-пароль отличается от пароля Nextcloud и нужен на принимающем устройстве. Для экспорта — минимум 12 символов. Он не сохраняется в настройках.", "Use a separate master password of at least 12 characters. Enter it again on the receiving device.")).build();
    let master = adw::PasswordEntryRow::builder()
        .title(tr!("Мастер-пароль набора", "Bundle master password"))
        .build();
    let confirm = adw::PasswordEntryRow::builder()
        .title(tr!(
            "Повтор мастер-пароля (для отправки)",
            "Repeat master password for upload"
        ))
        .build();
    group.add(&master);
    group.add(&confirm);
    body.append(&group);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let mut candidates = Vec::new();
    let status = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    match state.store.profiles() {
        Ok(profiles) => {
            for profile in profiles {
                let row = adw::ActionRow::builder()
                    .title(&profile.name)
                    .subtitle(tr_format!(
                        "{} · порт {}",
                        "{} · port {}",
                        profile.host,
                        profile.port
                    ))
                    .build();
                row.set_use_markup(false);
                let check = gtk::CheckButton::builder()
                    .active(true)
                    .valign(gtk::Align::Center)
                    .build();
                row.add_prefix(&check);
                row.set_activatable_widget(Some(&check));
                list.append(&row);
                candidates.push((check, PortableProfile::from(&profile)));
            }
        }
        Err(_) => notify(
            &status,
            tr!(
                "Не удалось прочитать локальные профили.",
                "Could not read local profiles."
            ),
            false,
        ),
    }
    body.append(&gtk::Label::new(Some(tr!(
        "Подключения для отправки",
        "Connections to Upload"
    ))));
    let selection = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for (label, active) in [
        (tr!("Выбрать все", "Select All"), true),
        (tr!("Снять отметки", "Clear Selection"), false),
    ] {
        let button = gtk::Button::with_label(label);
        let checks: Vec<_> = candidates.iter().map(|(check, _)| check.clone()).collect();
        button.connect_clicked(move |_| {
            for check in &checks {
                check.set_active(active);
            }
        });
        selection.append(&button);
    }
    body.append(&selection);
    body.append(&list);
    let refresh = gtk::Button::with_label(tr!(
        "Проверить подключение и обновить список наборов",
        "Test Connection and Refresh Bundles"
    ));
    body.append(&refresh);
    let remote = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&[]))
        .build();
    remote.set_tooltip_text(Some(tr!(
        "Набор для загрузки; новые наборы показаны первыми",
        "Bundle to download, newest first"
    )));
    body.append(&remote);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&body)
        .build();
    root.append(&scroll);
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    footer.set_margin_start(16);
    footer.set_margin_end(16);
    footer.set_margin_top(8);
    footer.set_margin_bottom(16);
    footer.append(&status);
    let upload = gtk::Button::with_label(tr!(
        "Отправить отмеченные подключения",
        "Upload Selected Connections"
    ));
    upload.add_css_class("suggested-action");
    let download = gtk::Button::with_label(tr!(
        "Открыть выбранный набор с сервера",
        "Open Selected Bundle"
    ));
    footer.append(&upload);
    footer.append(&download);
    root.append(&footer);
    if let Err(error) = settings {
        notify(&status, &error, false);
    }
    let form = Rc::new(Form {
        dialog: dialog.downgrade(),
        root: root.downgrade(),
        directory,
        username,
        token,
        master,
        confirm,
        status,
        remote,
        snapshots: RefCell::new(Vec::new()),
        listed_for: RefCell::new(None),
        saved: RefCell::new(saved),
        candidates,
        state,
    });
    for (button, action) in [
        (save, Action::Save),
        (refresh, Action::List),
        (upload, Action::Upload),
        (download, Action::Download),
    ] {
        let form = form.clone();
        button.connect_clicked(move |_| start(form.clone(), action));
    }
    dialog.set_child(Some(&root));
    dialog.present(Some(parent));
}

async fn resolve(
    mut settings: Settings,
    token: Zeroizing<String>,
    saved: Option<Settings>,
) -> transfer::Result<(Settings, Zeroizing<String>)> {
    let matches = saved
        .as_ref()
        .is_some_and(|s| s.directory == settings.directory && s.username == settings.username);
    if matches {
        settings.uid = saved.as_ref().unwrap().uid.clone();
    }
    if !token.is_empty() {
        return Ok((settings, token));
    }
    if !matches {
        return Err(tr!("Введите пароль приложения для этого URL и пользователя. Пароль от другого хранилища не используется.", "Enter the app password for this URL and username.").into());
    }
    let token = secrets::lookup_webdav(&settings.uid)
        .await
        .map_err(String::from)?
        .ok_or(tr!(
            "Сохранённого пароля WebDAV на этом ПК нет.",
            "No WebDAV password is saved on this computer."
        ))?;
    Ok((settings, token))
}

fn start(form: Rc<Form>, action: Action) {
    let (Some(dialog), Some(root)) = (form.dialog.upgrade(), form.root.upgrade()) else {
        return;
    };
    let settings = match Settings::new(&form.directory.text(), &form.username.text()) {
        Ok(settings) => settings,
        Err(error) => {
            notify(&form.status, &error, false);
            return;
        }
    };
    let token = Zeroizing::new(form.token.text().to_string());
    let master = Zeroizing::new(form.master.text().to_string());
    if matches!(action, Action::Upload)
        && (master.chars().count() < 12
            || master.len() > 1024
            || master.as_str() != form.confirm.text().as_str())
    {
        notify(&form.status, tr!("Для отправки введите и повторите мастер-пароль: минимум 12 символов, максимум 1024 байта.", "Enter and repeat a master password with at least 12 characters and at most 1,024 bytes."), false);
        return;
    }
    if matches!(action, Action::Download) && (master.is_empty() || master.len() > 1024) {
        notify(
            &form.status,
            tr!(
                "Введите мастер-пароль выбранного набора.",
                "Enter the master password for the selected bundle."
            ),
            false,
        );
        return;
    }
    let profiles: Vec<_> = form
        .candidates
        .iter()
        .filter(|(check, _)| check.is_active())
        .map(|(_, p)| p.clone())
        .collect();
    if matches!(action, Action::Upload) {
        if profiles.is_empty() {
            notify(
                &form.status,
                tr!(
                    "Отметьте хотя бы одно подключение.",
                    "Select at least one connection."
                ),
                false,
            );
            return;
        }
        match form.state.store.profiles() {
            Ok(current)
                if profiles.iter().all(|p| {
                    current
                        .iter()
                        .any(|local| p.uid == local.uid && p.same_fields(local))
                }) => {}
            _ => {
                notify(
                    &form.status,
                    tr!(
                        "Локальные профили изменились. Откройте окно WebDAV заново.",
                        "Local profiles have changed. Reopen the WebDAV dialog."
                    ),
                    false,
                );
                return;
            }
        }
    }
    let selected = if matches!(action, Action::Download) {
        let snapshots = form.snapshots.borrow();
        match snapshots.get(form.remote.selected() as usize) {
            Some(snapshot) => Some(snapshot.name.clone()),
            None => {
                notify(
                    &form.status,
                    tr!(
                        "Сначала обновите список и выберите набор.",
                        "Refresh the list and select a bundle."
                    ),
                    false,
                );
                return;
            }
        }
    } else {
        None
    };
    let saved = form.saved.borrow().clone();
    // A list belongs to the endpoint used to fetch it; do not fetch it from a changed endpoint.
    if matches!(action, Action::Download)
        && form.listed_for.borrow().as_ref()
            != Some(&(settings.directory.clone(), settings.username.clone()))
    {
        notify(
            &form.status,
            tr!(
                "Адрес хранилища изменился. Обновите список наборов.",
                "The storage URL changed. Refresh the bundle list."
            ),
            false,
        );
        return;
    }
    root.set_sensitive(false);
    dialog.set_can_close(false);
    form.status.remove_css_class("error");
    form.status.remove_css_class("success");
    form.status
        .set_text(tr!("Выполняется операция…", "Working…"));
    form.status.set_visible(true);
    glib::MainContext::default().spawn_local(async move {
        let result = async {
            let (settings, token) = resolve(settings, token, saved.clone()).await?;
            if matches!(action, Action::Save) {
                secrets::store_webdav(&settings.uid, &token).await.map_err(String::from)?;
                settings.save()?;
                let old = form.saved.replace(Some(settings.clone()));
                if let Some(old) = old.filter(|old| old.uid != settings.uid) {
                    if secrets::clear_webdav(&old.uid).await.is_err() {
                        return Ok((None, tr!("Настройки сохранены. Старый пароль WebDAV не удалось удалить из Secret Service.", "Settings saved. Could not remove the old WebDAV password from Secret Service.").into()));
                    }
                }
                form.token.set_text("");
                return Ok((None, tr!("URL и пользователь сохранены в настройках; пароль приложения — только в локальном Secret Service.", "URL and username saved. The app password is stored in the local Secret Service.").into()));
            }
            let endpoint = (settings.directory.clone(), settings.username.clone());
            let result = gio::spawn_blocking(move || -> transfer::Result<Outcome> {
                let dav = Dav::new(settings, token)?;
                match action {
                    Action::List => Ok(Outcome::Listed(dav.list()?)),
                    Action::Upload => Ok(Outcome::Uploaded(dav.upload(transfer::encrypt(profiles, &master)?)?)),
                    Action::Download => Ok(Outcome::Downloaded(transfer::decrypt(&dav.download(&selected.unwrap())?, &master)?)),
                    Action::Save => unreachable!(),
                }
            }).await.map_err(|_| tr!("Операция WebDAV прервана.", "WebDAV operation interrupted."))??;
            if matches!(result, Outcome::Listed(_)) { form.listed_for.replace(Some(endpoint)); }
            Ok::<_, String>((Some(result), String::new()))
        }.await;
        root.set_sensitive(true); dialog.set_can_close(true);
        match result {
            Ok((None, text)) => notify(&form.status, &text, true),
            Ok((Some(Outcome::Listed(snapshots)), _)) => {
                let labels: Vec<_> = snapshots.iter().map(|s| format!("{} · {}", s.modified, s.name)).collect();
                let labels: Vec<_> = labels.iter().map(String::as_str).collect();
                form.remote.set_model(Some(&gtk::StringList::new(&labels)));
                notify(&form.status, &tr_format!("Подключение успешно. Найдено наборов: {}.", "Connected. Bundles found: {}.", snapshots.len()), true);
                form.snapshots.replace(snapshots);
            }
            Ok((Some(Outcome::Uploaded(name)), _)) => notify(&form.status, &tr_format!("Зашифрованный набор отправлен: {name}. Обновите список для его открытия.", "Encrypted bundle uploaded: {name}. Refresh the list to open it."), true),
            Ok((Some(Outcome::Downloaded(profiles)), _)) => { notify(&form.status, tr!("Набор загружен и расшифрован. Выберите подключения в окне просмотра.", "Bundle downloaded and decrypted. Select connections in the preview."), true); transfer_ui::preview(&dialog, form.state.clone(), profiles); }
            Err(error) => notify(&form.status, &error, false),
        }
    });
}
