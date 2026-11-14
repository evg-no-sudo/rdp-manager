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
        .label("Ручной зашифрованный обмен наборами. Пароли RDP не отправляются. Каждый экспорт создаёт новый набор; автоматического слияния и переноса удалений пока нет. Сначала создайте отдельную папку в Nextcloud и укажите её WebDAV URL.").build();
    body.append(&notice);
    let settings = Settings::load();
    let saved = settings.as_ref().ok().and_then(|value| value.clone());
    let group = adw::PreferencesGroup::builder().title("Хранилище")
        .description("Пример: https://cloud.example/remote.php/dav/files/USER/RdpManager/ — используйте пароль приложения Nextcloud. При изменении URL или пользователя введите пароль заново.").build();
    let directory = adw::EntryRow::builder()
        .title("HTTPS URL каталога")
        .text(saved.as_ref().map(|s| s.directory.as_str()).unwrap_or(""))
        .build();
    let username = adw::EntryRow::builder()
        .title("Пользователь WebDAV")
        .text(saved.as_ref().map(|s| s.username.as_str()).unwrap_or(""))
        .build();
    let token = adw::PasswordEntryRow::builder()
        .title("Пароль приложения (пусто — взять сохранённый)")
        .build();
    group.add(&directory);
    group.add(&username);
    group.add(&token);
    body.append(&group);
    let save = gtk::Button::with_label("Сохранить настройки и пароль приложения на этом ПК");
    body.append(&save);
    let group = adw::PreferencesGroup::builder().title("Шифрование набора")
        .description("Мастер-пароль отличается от пароля Nextcloud и нужен на принимающем устройстве. Для экспорта — минимум 12 символов. Он не сохраняется в настройках.").build();
    let master = adw::PasswordEntryRow::builder()
        .title("Мастер-пароль набора")
        .build();
    let confirm = adw::PasswordEntryRow::builder()
        .title("Повтор мастер-пароля (для отправки)")
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
                    .subtitle(format!("{} · порт {}", profile.host, profile.port))
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
        Err(_) => notify(&status, "Не удалось прочитать локальные профили.", false),
    }
    body.append(&gtk::Label::new(Some("Подключения для отправки")));
    let selection = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for (label, active) in [("Выбрать все", true), ("Снять отметки", false)] {
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
    let refresh = gtk::Button::with_label("Проверить подключение и обновить список наборов");
    body.append(&refresh);
    let remote = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&[]))
        .build();
    remote.set_tooltip_text(Some("Набор для загрузки; новые наборы показаны первыми"));
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
    let upload = gtk::Button::with_label("Отправить отмеченные подключения");
    upload.add_css_class("suggested-action");
    let download = gtk::Button::with_label("Открыть выбранный набор с сервера");
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
        return Err("Введите пароль приложения для этого URL и пользователя. Пароль от другого хранилища не используется.".into());
    }
    let token = secrets::lookup_webdav(&settings.uid)
        .await
        .map_err(String::from)?
        .ok_or("Сохранённого пароля WebDAV на этом ПК нет.")?;
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
        notify(&form.status, "Для отправки введите и повторите мастер-пароль: минимум 12 символов, максимум 1024 байта.", false);
        return;
    }
    if matches!(action, Action::Download) && (master.is_empty() || master.len() > 1024) {
        notify(
            &form.status,
            "Введите мастер-пароль выбранного набора.",
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
            notify(&form.status, "Отметьте хотя бы одно подключение.", false);
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
                    "Локальные профили изменились. Откройте окно WebDAV заново.",
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
                    "Сначала обновите список и выберите набор.",
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
            "Адрес хранилища изменился. Обновите список наборов.",
            false,
        );
        return;
    }
    root.set_sensitive(false);
    dialog.set_can_close(false);
    form.status.remove_css_class("error");
    form.status.remove_css_class("success");
    form.status.set_text("Выполняется операция…");
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
                        return Ok((None, "Настройки сохранены. Старый пароль WebDAV не удалось удалить из Secret Service.".into()));
                    }
                }
                form.token.set_text("");
                return Ok((None, "URL и пользователь сохранены в настройках; пароль приложения — только в локальном Secret Service.".into()));
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
            }).await.map_err(|_| "Операция WebDAV прервана.")??;
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
                notify(&form.status, &format!("Подключение успешно. Найдено наборов: {}.", snapshots.len()), true);
                form.snapshots.replace(snapshots);
            }
            Ok((Some(Outcome::Uploaded(name)), _)) => notify(&form.status, &format!("Зашифрованный набор отправлен: {name}. Обновите список для его открытия."), true),
            Ok((Some(Outcome::Downloaded(profiles)), _)) => { notify(&form.status, "Набор загружен и расшифрован. Выберите подключения в окне просмотра.", true); transfer_ui::preview(&dialog, form.state.clone(), profiles); }
            Err(error) => notify(&form.status, &error, false),
        }
    });
}
