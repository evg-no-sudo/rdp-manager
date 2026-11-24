use crate::{
    profiles_ui,
    transfer::{self, Change, ImportAction, PortableProfile},
    ui::State,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;
use zeroize::Zeroizing;

fn message(label: &gtk::Label, text: &str, success: bool) {
    label.remove_css_class("success");
    label.remove_css_class("error");
    label.add_css_class(if success { "success" } else { "error" });
    label.set_text(text);
    label.set_visible(true);
}

fn content() -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(16);
    content.set_margin_end(16);
    content.set_margin_top(8);
    content.set_margin_bottom(16);
    content
}

fn note(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .build()
}

fn scroll(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

pub fn show(parent: &adw::ApplicationWindow, state: Rc<State>) {
    let dialog = adw::Dialog::builder()
        .title(tr!(
            "Обмен профилями через папку",
            "Exchange Profiles via Folder"
        ))
        .content_width(620)
        .content_height(650)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&adw::HeaderBar::new());
    let body = content();
    body.append(&note(tr!("Зашифрованный перенос выбранных профилей. Пароли RDP не включаются. Пока это ручной обмен: каждый экспорт создаёт отдельный файл; автоматической синхронизации и переноса удалений ещё нет.", "Export or import encrypted profile bundles. Each export creates a separate file. RDP passwords stay on this computer.")));
    let group = adw::PreferencesGroup::new();
    let password = adw::PasswordEntryRow::builder()
        .title(tr!("Мастер-пароль набора", "Bundle master password"))
        .build();
    let confirm = adw::PasswordEntryRow::builder()
        .title(tr!(
            "Повтор мастер-пароля (для экспорта)",
            "Repeat master password for export"
        ))
        .build();
    group.add(&password);
    group.add(&confirm);
    group.set_description(Some(tr!("Для нового набора — минимум 12 символов. На другом ПК понадобится тот же мастер-пароль. Он не сохраняется в файле или настройках.", "Use at least 12 characters for a new bundle. Enter the same master password on the other computer.")));
    body.append(&group);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let status = note("");
    status.set_visible(false);
    let profiles = match state.store.profiles() {
        Ok(profiles) => profiles,
        Err(_) => {
            message(
                &status,
                tr!(
                    "Не удалось прочитать профили из базы.",
                    "Could not read profiles from the database."
                ),
                false,
            );
            Vec::new()
        }
    };
    let candidates: Rc<Vec<_>> = Rc::new(
        profiles
            .iter()
            .map(|p| {
                let row = adw::ActionRow::builder()
                    .title(&p.name)
                    .subtitle(tr_format!(
                        "{} · порт {} · {}",
                        "{} · port {} · {}",
                        p.host,
                        p.port,
                        p.username
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
                (check, PortableProfile::from(p))
            })
            .collect(),
    );
    let selection = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for (label, active) in [
        (tr!("Выбрать все", "Select All"), true),
        (tr!("Снять отметки", "Clear Selection"), false),
    ] {
        let button = gtk::Button::with_label(label);
        let candidates = candidates.clone();
        button.connect_clicked(move |_| {
            for (check, _) in candidates.iter() {
                check.set_active(active);
            }
        });
        selection.append(&button);
    }
    body.append(&selection);
    body.append(&list);
    root.append(&scroll(&body));
    let footer = content();
    footer.append(&status);
    let export = gtk::Button::with_label(tr!(
        "Сохранить выбранные в папку…",
        "Save Selected to Folder…"
    ));
    export.add_css_class("suggested-action");
    export.set_sensitive(!candidates.is_empty());
    footer.append(&export);
    let import = gtk::Button::with_label(tr!(
        "Открыть зашифрованный набор…",
        "Open Encrypted Bundle…"
    ));
    footer.append(&import);
    root.append(&footer);

    let weak_dialog = dialog.downgrade();
    let weak_parent = parent.downgrade();
    let export_root = root.downgrade();
    let export_status = status.clone();
    let export_password = password.clone();
    let export_state = state.clone();
    export.connect_clicked(move |_| {
        let (Some(dialog), Some(parent)) = (weak_dialog.upgrade(), weak_parent.upgrade()) else { return; };
        let password = Zeroizing::new(export_password.text().to_string());
        if password.chars().count() < 12 || password.len() > 1024 {
            message(&export_status, tr!("Введите мастер-пароль: минимум 12 символов, максимум 1024 байта.", "Enter a master password with at least 12 characters and at most 1,024 bytes."), false); return;
        }
        if password.as_str() != confirm.text().as_str() {
            message(&export_status, tr!("Мастер-пароли не совпадают.", "The master passwords do not match."), false); return;
        }
        let profiles: Vec<_> = candidates.iter().filter(|(check, _)| check.is_active()).map(|(_, p)| p.clone()).collect();
        if profiles.is_empty() { message(&export_status, tr!("Отметьте хотя бы один профиль.", "Select at least one profile."), false); return; }
        let current = match export_state.store.profiles() {
            Ok(profiles) => profiles,
            Err(_) => { message(&export_status, tr!("Не удалось прочитать профили.", "Could not read profiles."), false); return; }
        };
        if profiles.iter().any(|p| !current.iter().any(|local| p.uid == local.uid && p.same_fields(local))) {
            message(&export_status, tr!("Профили изменились в другом окне. Откройте обмен заново.", "Profiles changed in another window. Reopen the exchange dialog."), false); return;
        }
        let chooser = gtk::FileDialog::builder().title(tr!("Папка для зашифрованного набора", "Folder for Encrypted Bundle")).modal(true).build();
        let choose = chooser.select_folder_future(Some(&parent));
        let Some(root) = export_root.upgrade() else { return; };
        let status = export_status.clone();
        root.set_sensitive(false);
        dialog.set_can_close(false);
        glib::MainContext::default().spawn_local(async move {
            match choose.await {
                Ok(folder) => {
                    if let Some(path) = folder.path() {
                        let result = gio::spawn_blocking(move || transfer::publish(&path, profiles, &password)).await
                            .unwrap_or_else(|_| Err(tr!("Операция экспорта прервана.", "Export interrupted.").into()));
                        match result {
                            Ok(path) => message(&status, &tr_format!("Набор сохранён: {}", "Bundle saved: {}", path.display()), true),
                            Err(error) => message(&status, &error, false),
                        }
                    } else { message(&status, tr!("Выберите локально доступную папку. Прямой доступ к сетевым хранилищам будет добавлен отдельно.", "Choose a folder accessible from this computer."), false); }
                }
                Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
                Err(_) => message(&status, tr!("Не удалось выбрать папку.", "Could not select the folder."), false),
            }
            root.set_sensitive(true);
            dialog.set_can_close(true);
        });
    });

    let weak_dialog = dialog.downgrade();
    let weak_parent = parent.downgrade();
    let import_root = root.downgrade();
    import.connect_clicked(move |_| {
        let (Some(dialog), Some(parent)) = (weak_dialog.upgrade(), weak_parent.upgrade()) else {
            return;
        };
        let password = Zeroizing::new(password.text().to_string());
        if password.is_empty() || password.len() > 1024 {
            message(
                &status,
                tr!(
                    "Введите мастер-пароль открываемого набора.",
                    "Enter the master password for the bundle."
                ),
                false,
            );
            return;
        }
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(tr!(
            "Зашифрованные профили (*.rdpm)",
            "Encrypted profiles (*.rdpm)"
        )));
        filter.add_pattern("*.rdpm");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let chooser = gtk::FileDialog::builder()
            .title(tr!("Открыть набор профилей", "Open Profile Bundle"))
            .modal(true)
            .filters(&filters)
            .default_filter(&filter)
            .build();
        let choose = chooser.open_future(Some(&parent));
        let Some(root) = import_root.upgrade() else {
            return;
        };
        let status = status.clone();
        let state = state.clone();
        root.set_sensitive(false);
        dialog.set_can_close(false);
        glib::MainContext::default().spawn_local(async move {
            let mut imported = None;
            match choose.await {
                Ok(file) => {
                    if let Some(path) = file.path() {
                        let result = gio::spawn_blocking(move || transfer::read(&path, &password))
                            .await
                            .unwrap_or_else(|_| {
                                Err(tr!("Операция чтения прервана.", "Read interrupted.").into())
                            });
                        match result {
                            Ok(profiles) => {
                                imported = Some(profiles);
                            }
                            Err(error) => message(&status, &error, false),
                        }
                    } else {
                        message(
                            &status,
                            tr!(
                                "Выберите локально доступный файл набора.",
                                "Choose a bundle file accessible from this computer."
                            ),
                            false,
                        );
                    }
                }
                Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
                Err(_) => message(
                    &status,
                    tr!("Не удалось выбрать файл.", "Could not select the file."),
                    false,
                ),
            }
            root.set_sensitive(true);
            dialog.set_can_close(true);
            if let Some(profiles) = imported {
                preview(&dialog, state, profiles);
            }
        });
    });
    dialog.set_child(Some(&root));
    dialog.present(Some(parent));
}

pub fn preview(parent: &adw::Dialog, state: Rc<State>, profiles: Vec<PortableProfile>) {
    let dialog = adw::Dialog::builder()
        .title(tr!("Просмотр набора профилей", "Preview Profile Bundle"))
        .content_width(680)
        .content_height(620)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&adw::HeaderBar::new());
    let body = content();
    body.append(&note(tr!("Для отличающихся профилей показано, что изменится: слева текущее значение, справа значение из набора. Отметьте профиль, чтобы сохранить копию или заменить локальную версию. Если меняется сервер или пользователь, проверьте сохранённый пароль.", "Changes are shown as current value → imported value. Select a profile to keep a copy or replace the local version. If the server or username changes, check the saved password.")));
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let footer = content();
    let status = note("");
    status.set_visible(false);
    let local = match state.store.profiles() {
        Ok(local) => local,
        Err(_) => {
            message(
                &status,
                tr!(
                    "Не удалось прочитать локальные профили.",
                    "Could not read local profiles."
                ),
                false,
            );
            footer.append(&status);
            root.append(&footer);
            dialog.set_child(Some(&root));
            dialog.present(Some(parent));
            return;
        }
    };
    let mut candidates = Vec::new();
    let mut seen: Vec<PortableProfile> = Vec::new();
    for incoming in profiles {
        let existing = local.iter().find(|p| p.uid == incoming.uid);
        let unchanged = existing.is_some_and(|p| incoming.same_fields(p));
        let duplicate = existing.is_none()
            && (local.iter().any(|p| incoming.same_fields(p))
                || seen.iter().any(|p| incoming.same_fields(&p.profile())));
        let conflict = existing.is_some() && !unchanged;
        let label = if unchanged || duplicate {
            tr!("Уже есть — без изменений", "Already present")
        } else if conflict {
            tr!(
                "Отличается от локального профиля",
                "Differs from the local profile"
            )
        } else {
            tr!("Новый профиль", "New profile")
        };
        let row = adw::ActionRow::builder()
            .title(&incoming.name)
            .subtitle(tr_format!(
                "{label}\n{} · порт {}",
                "{label}\n{} · port {}",
                incoming.host,
                incoming.port
            ))
            .build();
        row.set_use_markup(false);
        let check = gtk::CheckButton::builder()
            .active(!conflict && !unchanged && !duplicate)
            .sensitive(!unchanged && !duplicate)
            .valign(gtk::Align::Center)
            .build();
        row.add_prefix(&check);
        row.set_activatable_widget(Some(&check));
        let action = gtk::DropDown::from_strings(&[
            tr!("Сохранить копию", "Keep a Copy"),
            tr!("Заменить локальный", "Replace Local Profile"),
        ]);
        action.set_valign(gtk::Align::Center);
        if conflict {
            row.add_suffix(&action);
        }
        seen.push(incoming.clone());
        if let Some(local) = existing.filter(|_| conflict) {
            let group = gtk::Box::new(gtk::Orientation::Vertical, 0);
            group.append(&row);
            let mut fields = vec![
                (
                    tr!("Название", "Name"),
                    local.name.as_str(),
                    incoming.name.as_str(),
                ),
                (
                    tr!("Пользователь", "Username"),
                    local.username.as_str(),
                    incoming.username.as_str(),
                ),
                (
                    tr!("Домен", "Domain"),
                    local.domain.as_str(),
                    incoming.domain.as_str(),
                ),
            ];
            if !local.host.eq_ignore_ascii_case(&incoming.host) {
                fields.push((
                    tr!("Сервер", "Server"),
                    local.host.as_str(),
                    incoming.host.as_str(),
                ));
            }
            let local_port = local.port.to_string();
            let incoming_port = incoming.port.to_string();
            fields.push((tr!("Порт", "Port"), &local_port, &incoming_port));
            for (field, current, incoming) in fields.into_iter().filter(|(_, a, b)| a != b) {
                let current = if current.is_empty() { "—" } else { current };
                let incoming = if incoming.is_empty() { "—" } else { incoming };
                let detail = gtk::Label::new(Some(&format!("{field}: {current} → {incoming}")));
                detail.set_wrap(true);
                detail.set_xalign(0.0);
                detail.set_margin_start(16);
                detail.set_margin_end(16);
                detail.set_margin_bottom(6);
                group.append(&detail);
            }
            list.append(&group);
        } else {
            list.append(&row);
        }
        candidates.push((check, action, incoming, existing.cloned()));
    }
    body.append(&list);
    root.append(&scroll(&body));
    footer.append(&status);
    let apply = gtk::Button::with_label(tr!("Применить отмеченные", "Apply Selected"));
    apply.add_css_class("suggested-action");
    footer.append(&apply);
    root.append(&footer);
    apply.connect_clicked(move |button| {
        let changes: Vec<_> = candidates
            .iter()
            .filter(|(check, _, _, _)| check.is_sensitive() && check.is_active())
            .map(|(_, action, incoming, existing)| Change {
                incoming: incoming.clone(),
                expected: existing.clone(),
                action: if existing.is_none() {
                    ImportAction::Add
                } else if action.selected() == 0 {
                    ImportAction::Copy
                } else {
                    ImportAction::Replace
                },
            })
            .collect();
        if changes.is_empty() {
            message(
                &status,
                tr!(
                    "Нет отмеченных новых или изменённых профилей.",
                    "Select at least one new or changed profile."
                ),
                false,
            );
            return;
        }
        let mut operations = Vec::new();
        for change in &changes {
            let Some(operation) = state.operation(&change.incoming.uid) else {
                message(
                    &status,
                    tr!(
                        "С одним из профилей выполняется операция в другом окне. Повторите позже.",
                        "Another window is using one of these profiles. Try again later."
                    ),
                    false,
                );
                return;
            };
            operations.push(operation);
        }
        match state.store.apply_transfer(&changes) {
            Ok((added, replaced, copied)) => {
                profiles_ui::refresh(&state);
                message(
                    &status,
                    &tr_format!(
                        "Добавлено: {added}. Заменено: {replaced}. Создано копий: {copied}.",
                        "Added: {added}. Replaced: {replaced}. Copies: {copied}."
                    ),
                    true,
                );
                button.set_sensitive(false);
                for (check, action, _, _) in &candidates {
                    check.set_sensitive(false);
                    action.set_sensitive(false);
                }
            }
            Err(error) => message(
                &status,
                &tr_format!("Перенос отменён: {error}", "Import cancelled: {error}"),
                false,
            ),
        }
    });
    dialog.set_child(Some(&root));
    dialog.present(Some(parent));
}
