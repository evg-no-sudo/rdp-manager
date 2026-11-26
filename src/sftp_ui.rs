use crate::{
    secrets,
    sftp::{Auth, Failure, HostIdentity, Remote, Settings},
    transfer::{self, PortableProfile, Snapshot},
    transfer_ui,
    ui::State,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::{cell::RefCell, rc::Rc};
use zeroize::Zeroizing;

struct Form {
    dialog: glib::WeakRef<adw::Dialog>,
    root: glib::WeakRef<gtk::Box>,
    host: adw::EntryRow,
    port: adw::EntryRow,
    username: adw::EntryRow,
    directory: adw::EntryRow,
    auth: gtk::DropDown,
    key: adw::EntryRow,
    secret: adw::PasswordEntryRow,
    remember: gtk::CheckButton,
    master: adw::PasswordEntryRow,
    confirm: adw::PasswordEntryRow,
    status: gtk::Label,
    remote: gtk::DropDown,
    snapshots: RefCell<Vec<Snapshot>>,
    listed_for: RefCell<Option<Settings>>,
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
    Saved(String),
    Listed(Vec<Snapshot>),
    Uploaded(String),
    Downloaded(Vec<PortableProfile>),
}

fn notify(label: &gtk::Label, text: &str, success: bool) {
    label.remove_css_class("success");
    label.remove_css_class("error");
    label.add_css_class(if success { "success" } else { "error" });
    label.set_text(text);
    label.set_visible(true);
}
fn entry(group: &adw::PreferencesGroup, title: &str, text: &str) -> adw::EntryRow {
    let row = adw::EntryRow::builder().title(title).text(text).build();
    group.add(&row);
    row
}
fn box_content() -> gtk::Box {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.set_margin_top(8);
    body.set_margin_bottom(16);
    body
}

pub fn show(parent: &adw::ApplicationWindow, state: Rc<State>) {
    let settings = Settings::load();
    let saved = settings.as_ref().ok().and_then(|v| v.clone());
    let dialog = adw::Dialog::builder()
        .title(tr!(
            "Синхронизация подключений через SFTP",
            "Sync Connections via SFTP"
        ))
        .content_width(580)
        .content_height(600)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&adw::HeaderBar::new());
    let body = box_content();
    let connection = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let connection_expander = gtk::Expander::builder()
        .label(tr!("Настройки SSH", "SSH Settings"))
        .expanded(saved.is_none())
        .child(&connection)
        .build();
    body.append(&connection_expander);
    let group = adw::PreferencesGroup::builder()
        .title(tr!("SSH-сервер", "SSH Server"))
        .build();
    let host = entry(
        &group,
        tr!("Сервер (DNS или IP)", "Server (DNS or IP)"),
        saved.as_ref().map(|s| s.host.as_str()).unwrap_or(""),
    );
    let port = entry(
        &group,
        tr!("Порт", "Port"),
        &saved.as_ref().map(|s| s.port).unwrap_or(22).to_string(),
    );
    let username = entry(
        &group,
        tr!("Пользователь SSH", "SSH username"),
        saved.as_ref().map(|s| s.username.as_str()).unwrap_or(""),
    );
    let directory = entry(
        &group,
        tr!("Папка на сервере", "Server folder"),
        saved
            .as_ref()
            .map(|s| s.directory.as_str())
            .unwrap_or("rdp-manager"),
    );
    connection.append(&group);
    let auth = gtk::DropDown::from_strings(&[
        "SSH-agent",
        tr!("Файл SSH-ключа", "SSH key file"),
        tr!("Пароль SSH", "SSH password"),
    ]);
    auth.set_selected(
        match saved.as_ref().map(|s| s.auth).unwrap_or(Auth::Agent) {
            Auth::Agent => 0,
            Auth::KeyFile => 1,
            Auth::Password => 2,
        },
    );
    let auth_row = adw::ActionRow::builder()
        .title(tr!("Способ входа", "Authentication"))
        .build();
    auth.set_valign(gtk::Align::Center);
    auth_row.add_suffix(&auth);
    group.add(&auth_row);
    let group = adw::PreferencesGroup::new();
    let key = entry(
        &group,
        tr!("Файл приватного ключа", "Private key file"),
        saved.as_ref().map(|s| s.key_file.as_str()).unwrap_or(""),
    );
    let secret = adw::PasswordEntryRow::builder()
        .title(tr!("Passphrase ключа", "Key passphrase"))
        .build();
    group.add(&secret);
    connection.append(&group);
    let choose_key = gtk::Button::with_label(tr!("Выбрать файл SSH-ключа…", "Choose SSH Key…"));
    connection.append(&choose_key);
    let weak_parent = parent.downgrade();
    let weak_key = key.downgrade();
    choose_key.connect_clicked(move |_| {
        if let Some(parent) = weak_parent.upgrade() {
            let future = gtk::FileDialog::builder()
                .title(tr!("Приватный SSH-ключ", "Private SSH Key"))
                .modal(true)
                .build()
                .open_future(Some(&parent));
            let weak_key = weak_key.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Ok(file) = future.await {
                    if let (Some(row), Some(path)) = (weak_key.upgrade(), file.path()) {
                        row.set_text(&path.to_string_lossy());
                    }
                }
            });
        }
    });
    let remember =
        gtk::CheckButton::with_label(tr!("Запомнить на этом ПК", "Remember on this computer"));
    remember.set_active(saved.as_ref().is_some_and(|s| s.secret_saved));
    connection.append(&remember);
    {
        let group = group.clone();
        let key = key.clone();
        let secret = secret.clone();
        let choose = choose_key.clone();
        let remember = remember.clone();
        let update = move |auth: &gtk::DropDown| {
            let mode = auth.selected();
            secret.set_text("");
            group.set_visible(mode != 0);
            key.set_visible(mode == 1);
            choose.set_visible(mode == 1);
            secret.set_visible(mode != 0);
            remember.set_visible(mode != 0);
            secret.set_title(if mode == 2 {
                tr!(
                    "Пароль SSH (пусто — взять сохранённый)",
                    "SSH password (leave blank to use saved password)"
                )
            } else {
                tr!(
                    "Passphrase ключа (пусто — сохранённая или ключ без пароля)",
                    "Key passphrase (leave blank if saved or unnecessary)"
                )
            });
        };
        update(&auth);
        auth.connect_selected_notify(update);
    }
    let save = gtk::Button::with_label(tr!("Сохранить настройки SFTP", "Save SFTP Settings"));
    connection.append(&save);
    let group = adw::PreferencesGroup::builder()
        .title(tr!("Шифрование подключений", "Profile Encryption"))
        .description(tr!(
            "Используйте один мастер-пароль на обоих ПК. Пароли RDP хранятся локально.",
            "Use the same master password on both computers. RDP passwords stay local."
        ))
        .build();
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
    let status = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    let mut candidates = Vec::new();
    match state.store.profiles() {
        Ok(profiles) => {
            for p in profiles {
                let row = adw::ActionRow::builder()
                    .title(&p.name)
                    .subtitle(tr_format!("{} · порт {}", "{} · port {}", p.host, p.port))
                    .build();
                row.set_use_markup(false);
                let check = gtk::CheckButton::builder()
                    .active(true)
                    .valign(gtk::Align::Center)
                    .build();
                row.add_prefix(&check);
                row.set_activatable_widget(Some(&check));
                list.append(&row);
                candidates.push((check, PortableProfile::from(&p)));
            }
        }
        Err(_) => notify(
            &status,
            tr!(
                "Не удалось прочитать локальные подключения.",
                "Could not read local connections."
            ),
            false,
        ),
    }
    let outgoing = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let selection = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for (text, active) in [
        (tr!("Выбрать все", "Select All"), true),
        (tr!("Снять отметки", "Clear Selection"), false),
    ] {
        let button = gtk::Button::with_label(text);
        let checks: Vec<_> = candidates.iter().map(|(check, _)| check.clone()).collect();
        button.connect_clicked(move |_| {
            for check in &checks {
                check.set_active(active);
            }
        });
        selection.append(&button);
    }
    outgoing.append(&selection);
    outgoing.append(&list);
    body.append(
        &gtk::Expander::builder()
            .label(tr!(
                "Выбрать подключения для отправки",
                "Choose Connections to Upload"
            ))
            .child(&outgoing)
            .build(),
    );
    let refresh = gtk::Button::with_label(tr!("Обновить список на сервере", "Refresh Server List"));
    body.append(&refresh);
    let remote = gtk::DropDown::builder()
        .model(&gtk::StringList::new(&[]))
        .build();
    body.append(&remote);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&body)
        .build();
    root.append(&scroll);
    let footer = box_content();
    footer.append(&status);
    let upload = gtk::Button::with_label(tr!("Отправить подключения", "Upload Connections"));
    upload.add_css_class("suggested-action");
    let download = gtk::Button::with_label(tr!("Предпросмотр импорта", "Preview Import"));
    footer.append(&upload);
    footer.append(&download);
    root.append(&footer);
    if let Err(error) = settings {
        notify(&status, &error, false);
    }
    let form = Rc::new(Form {
        dialog: dialog.downgrade(),
        root: root.downgrade(),
        host,
        port,
        username,
        directory,
        auth,
        key,
        secret,
        remember,
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
    secret: Zeroizing<String>,
    saved: Option<Settings>,
) -> crate::sftp::Result<(Settings, Zeroizing<String>)> {
    let matches = saved.as_ref().is_some_and(|s| s.same_login(&settings));
    if matches {
        settings.uid = saved.as_ref().unwrap().uid.clone();
    }
    if settings.auth == Auth::Agent {
        return Ok((settings, Zeroizing::new(String::new())));
    }
    if !secret.is_empty() {
        return Ok((settings, secret));
    }
    if matches && saved.as_ref().unwrap().secret_saved {
        let secret = secrets::lookup_sftp(&settings.uid).await?.ok_or(tr!(
            "Сохранённого пароля/passphrase SFTP на этом ПК нет.",
            "No SFTP password or passphrase is saved on this computer."
        ))?;
        return Ok((settings, secret));
    }
    if settings.auth == Auth::Password {
        return Err(tr!("Введите пароль SSH. При смене сервера, пользователя или способа входа старый секрет не подставляется.", "Enter the SSH password for the current server and username.").into());
    }
    Ok((settings, secret))
}

fn trust(form: Rc<Form>, identity: HostIdentity) {
    let Some(parent) = form.dialog.upgrade() else {
        return;
    };
    let alert=adw::AlertDialog::builder().heading(tr!("Новый SSH-сервер", "New SSH Server"))
        .body(tr_format!("{}:{}\nТип ключа: {}\n{}\n\nСверьте отпечаток с администратором сервера. Авторизация ещё не выполнялась. Приватный ключ не отправляется на сервер. Доверять этому ключу?", "{}:{}\nKey type: {}\n{}\n\nVerify the fingerprint with your server administrator. Trust this key?",identity.host,identity.port,identity.algorithm,identity.fingerprint)).build();
    alert.add_responses(&[
        ("cancel", tr!("Отмена", "Cancel")),
        ("trust", tr!("Доверять этому ключу", "Trust This Key")),
    ]);
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");
    alert.connect_response(None, move |_, response| {
        if response == "trust" {
            match form
                .state
                .store
                .trust_sftp_host(&identity.host, identity.port, &identity.key)
            {
                Ok(()) => notify(
                    &form.status,
                    tr!(
                        "Ключ сервера сохранён локально. Повторите выбранную операцию.",
                        "Host key saved locally. Retry the selected action."
                    ),
                    true,
                ),
                Err(_) => notify(
                    &form.status,
                    tr!(
                        "Не удалось сохранить доверие к серверу; существующий ключ не заменён.",
                        "Could not save the host key. The existing key was retained."
                    ),
                    false,
                ),
            }
        }
    });
    alert.present(Some(&parent));
}

fn start(form: Rc<Form>, action: Action) {
    let (Some(dialog), Some(root)) = (form.dialog.upgrade(), form.root.upgrade()) else {
        return;
    };
    let port = match form.port.text().trim().parse::<u16>() {
        Ok(port) => port,
        Err(_) => {
            notify(
                &form.status,
                tr!(
                    "Порт должен быть от 1 до 65535.",
                    "Enter a port from 1 to 65535."
                ),
                false,
            );
            return;
        }
    };
    let mut settings = Settings {
        uid: glib::uuid_string_random().into(),
        host: form.host.text().into(),
        port,
        username: form.username.text().into(),
        directory: form.directory.text().into(),
        auth: match form.auth.selected() {
            1 => Auth::KeyFile,
            2 => Auth::Password,
            _ => Auth::Agent,
        },
        key_file: form.key.text().into(),
        secret_saved: false,
    };
    if let Err(error) = settings.validate() {
        notify(&form.status, &error, false);
        return;
    }
    let secret = Zeroizing::new(form.secret.text().to_string());
    let master = Zeroizing::new(form.master.text().to_string());
    if matches!(action, Action::Upload)
        && (master.chars().count() < 12
            || master.len() > 1024
            || master.as_str() != form.confirm.text().as_str())
    {
        notify(
            &form.status,
            tr!("Введите и повторите мастер-пароль набора: минимум 12 символов, максимум 1024 байта.", "Enter and repeat a master password with at least 12 characters and at most 1,024 bytes."),
            false,
        );
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
    let mut profiles: Vec<_> = form
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
        profiles = match form.state.store.transfer_export(profiles) {
            Ok(profiles) => profiles,
            Err(error) => {
                notify(&form.status, &error.to_string(), false);
                return;
            }
        };
    }
    let selected = if matches!(action, Action::Download) {
        if !form
            .listed_for
            .borrow()
            .as_ref()
            .is_some_and(|old| old.same_location(&settings))
        {
            notify(
                &form.status,
                tr!(
                    "Настройки SFTP изменились. Обновите список наборов.",
                    "SFTP settings have changed. Refresh the bundle list."
                ),
                false,
            );
            return;
        }
        match form.snapshots.borrow().get(form.remote.selected() as usize) {
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
    let pinned = match form
        .state
        .store
        .sftp_host_key(&settings.host, settings.port)
    {
        Ok(key) => key,
        Err(_) => {
            notify(
                &form.status,
                tr!(
                    "Не удалось прочитать локальное доверие SSH.",
                    "Could not read the trusted SSH host key."
                ),
                false,
            );
            return;
        }
    };
    let saved = form.saved.borrow().clone();
    let remember = form.remember.is_active();
    root.set_sensitive(false);
    dialog.set_can_close(false);
    form.status.remove_css_class("error");
    form.status.remove_css_class("success");
    form.status.set_text(tr!(
        "Выполняется операция SFTP…",
        "SFTP operation in progress…"
    ));
    form.status.set_visible(true);
    glib::MainContext::default().spawn_local(async move{
        let result=async{
            let (mut settings,secret)=resolve(settings,secret,saved).await?;
            if matches!(action,Action::Save){
                settings.secret_saved=remember&&settings.auth!=Auth::Agent&&!secret.is_empty();
                if settings.secret_saved{secrets::store_sftp(&settings.uid,&secret).await?;}
                settings.save()?;
                let old=form.saved.replace(Some(settings.clone()));
                let mut message=tr!("Настройки SFTP сохранены. Приватный ключ остаётся в исходном файле; секреты — только в Secret Service.", "SFTP settings saved. The private key stays in its original file, and saved credentials stay in Secret Service.").to_string();
                if let Some(old)=old.filter(|old|old.secret_saved&&(old.uid!=settings.uid||!settings.secret_saved)){
                    if secrets::clear_sftp(&old.uid).await.is_err(){message.push_str(tr!(" Старую запись Secret Service не удалось удалить.", " Could not remove the old Secret Service entry."));}
                }
                if settings.secret_saved{form.secret.set_text("");}
                return Ok(Outcome::Saved(message));
            }
            let location=settings.clone();
            let result=gio::spawn_blocking(move||->crate::sftp::Result<Outcome>{
                let remote=Remote::connect(settings,secret,pinned)?;
                match action{
                    Action::List=>Ok(Outcome::Listed(remote.list()?)),
                    Action::Upload=>Ok(Outcome::Uploaded(remote.upload(transfer::encrypt(profiles,&master)?)?)),
                    Action::Download=>Ok(Outcome::Downloaded(transfer::decrypt(&remote.download(&selected.unwrap())?,&master)?)),
                    Action::Save=>unreachable!(),
                }
            }).await.map_err(|_|Failure::Message(tr!("Операция SFTP прервана.", "SFTP operation interrupted.").into()))??;
            if matches!(result,Outcome::Listed(_)){form.listed_for.replace(Some(location));}Ok::<_,Failure>(result)
        }.await;
        root.set_sensitive(true);dialog.set_can_close(true);
        match result{
            Ok(Outcome::Saved(message))=>notify(&form.status,&message,true),
            Ok(Outcome::Listed(snapshots))=>{
                let labels:Vec<_>=snapshots.iter().map(|s|format!("{} · {}",s.modified,s.name)).collect();let labels:Vec<_>=labels.iter().map(String::as_str).collect();
                form.remote.set_model(Some(&gtk::StringList::new(&labels)));notify(&form.status,&tr_format!("SFTP подключён. Найдено наборов: {}.", "SFTP connected. Bundles found: {}.",snapshots.len()),true);form.snapshots.replace(snapshots);
            }
            Ok(Outcome::Uploaded(name))=>notify(&form.status,&tr_format!("Зашифрованный набор отправлен: {name}. Обновите список для его открытия.", "Encrypted bundle uploaded: {name}. Refresh the list to open it."),true),
            Ok(Outcome::Downloaded(profiles))=>{notify(&form.status,tr!("Набор загружен и расшифрован.", "Bundle downloaded and decrypted."),true);transfer_ui::preview(&dialog,form.state.clone(),profiles);}
            Err(Failure::Message(message))=>notify(&form.status,&message,false),
            Err(Failure::UnknownHost(identity))=>{notify(&form.status,tr!("Ключ нового SSH-сервера требует подтверждения.", "Confirm the new SSH host key."),false);trust(form,identity);}
        }
    });
}
