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
        .title("Обмен профилями через папку")
        .content_width(620)
        .content_height(650)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&adw::HeaderBar::new());
    let body = content();
    body.append(&note("Зашифрованный перенос выбранных профилей. Пароли RDP не включаются. Пока это ручной обмен: каждый экспорт создаёт отдельный файл; автоматической синхронизации и переноса удалений ещё нет."));
    let group = adw::PreferencesGroup::new();
    let password = adw::PasswordEntryRow::builder()
        .title("Мастер-пароль набора")
        .build();
    let confirm = adw::PasswordEntryRow::builder()
        .title("Повтор мастер-пароля (для экспорта)")
        .build();
    group.add(&password);
    group.add(&confirm);
    group.set_description(Some("Для нового набора — минимум 12 символов. На другом ПК понадобится тот же мастер-пароль. Он не сохраняется в файле или настройках."));
    body.append(&group);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let status = note("");
    status.set_visible(false);
    let profiles = match state.store.profiles() {
        Ok(profiles) => profiles,
        Err(_) => {
            message(&status, "Не удалось прочитать профили из базы.", false);
            Vec::new()
        }
    };
    let candidates: Rc<Vec<_>> = Rc::new(
        profiles
            .iter()
            .map(|p| {
                let row = adw::ActionRow::builder()
                    .title(&p.name)
                    .subtitle(format!("{} · порт {} · {}", p.host, p.port, p.username))
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
    for (label, active) in [("Выбрать все", true), ("Снять отметки", false)] {
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
    let export = gtk::Button::with_label("Сохранить выбранные в папку…");
    export.add_css_class("suggested-action");
    export.set_sensitive(!candidates.is_empty());
    footer.append(&export);
    let import = gtk::Button::with_label("Открыть зашифрованный набор…");
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
            message(&export_status, "Введите мастер-пароль: минимум 12 символов, максимум 1024 байта.", false); return;
        }
        if password.as_str() != confirm.text().as_str() {
            message(&export_status, "Мастер-пароли не совпадают.", false); return;
        }
        let profiles: Vec<_> = candidates.iter().filter(|(check, _)| check.is_active()).map(|(_, p)| p.clone()).collect();
        if profiles.is_empty() { message(&export_status, "Отметьте хотя бы один профиль.", false); return; }
        let current = match export_state.store.profiles() {
            Ok(profiles) => profiles,
            Err(_) => { message(&export_status, "Не удалось прочитать профили.", false); return; }
        };
        if profiles.iter().any(|p| !current.iter().any(|local| p.uid == local.uid && p.same_fields(local))) {
            message(&export_status, "Профили изменились в другом окне. Откройте обмен заново.", false); return;
        }
        let chooser = gtk::FileDialog::builder().title("Папка для зашифрованного набора").modal(true).build();
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
                            .unwrap_or_else(|_| Err("Операция экспорта прервана.".into()));
                        match result {
                            Ok(path) => message(&status, &format!("Набор сохранён: {}", path.display()), true),
                            Err(error) => message(&status, &error, false),
                        }
                    } else { message(&status, "Выберите локально доступную папку. Прямой доступ к сетевым хранилищам будет добавлен отдельно.", false); }
                }
                Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
                Err(_) => message(&status, "Не удалось выбрать папку.", false),
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
            message(&status, "Введите мастер-пароль открываемого набора.", false);
            return;
        }
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Зашифрованные профили (*.rdpm)"));
        filter.add_pattern("*.rdpm");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let chooser = gtk::FileDialog::builder()
            .title("Открыть набор профилей")
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
                            .unwrap_or_else(|_| Err("Операция чтения прервана.".into()));
                        match result {
                            Ok(profiles) => {
                                imported = Some(profiles);
                            }
                            Err(error) => message(&status, &error, false),
                        }
                    } else {
                        message(&status, "Выберите локально доступный файл набора.", false);
                    }
                }
                Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
                Err(_) => message(&status, "Не удалось выбрать файл.", false),
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

fn preview(parent: &adw::Dialog, state: Rc<State>, profiles: Vec<PortableProfile>) {
    let dialog = adw::Dialog::builder()
        .title("Просмотр набора профилей")
        .content_width(680)
        .content_height(620)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&adw::HeaderBar::new());
    let body = content();
    body.append(&note("Выберите нужные подключения. Изменённые профили по умолчанию не отмечены; для них можно сохранить копию или явно заменить локальную версию. Локальные пароли не изменяются: после замены сервера или пользователя проверьте их актуальность. Удаления из файла не выполняются."));
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let footer = content();
    let status = note("");
    status.set_visible(false);
    let local = match state.store.profiles() {
        Ok(local) => local,
        Err(_) => {
            message(&status, "Не удалось прочитать локальные профили.", false);
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
            "Уже есть — без изменений"
        } else if conflict {
            "Отличается от локального профиля"
        } else {
            "Новый профиль"
        };
        let row = adw::ActionRow::builder()
            .title(&incoming.name)
            .subtitle(format!(
                "{label}\n{} · порт {} · пользователь: {} · домен: {}",
                incoming.host, incoming.port, incoming.username, incoming.domain
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
        let action = gtk::DropDown::from_strings(&["Сохранить копию", "Заменить локальный"]);
        action.set_valign(gtk::Align::Center);
        if conflict {
            row.add_suffix(&action);
        }
        seen.push(incoming.clone());
        candidates.push((check, action, incoming, existing.cloned()));
        list.append(&row);
    }
    body.append(&list);
    root.append(&scroll(&body));
    footer.append(&status);
    let apply = gtk::Button::with_label("Применить отмеченные");
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
                "Нет отмеченных новых или изменённых профилей.",
                false,
            );
            return;
        }
        let mut operations = Vec::new();
        for change in &changes {
            let Some(operation) = state.operation(&change.incoming.uid) else {
                message(
                    &status,
                    "С одним из профилей выполняется операция в другом окне. Повторите позже.",
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
                    &format!("Добавлено: {added}. Заменено: {replaced}. Создано копий: {copied}."),
                    true,
                );
                button.set_sensitive(false);
                for (check, action, _, _) in &candidates {
                    check.set_sensitive(false);
                    action.set_sensitive(false);
                }
            }
            Err(error) => message(&status, &format!("Перенос отменён: {error}"), false),
        }
    });
    dialog.set_child(Some(&root));
    dialog.present(Some(parent));
}
