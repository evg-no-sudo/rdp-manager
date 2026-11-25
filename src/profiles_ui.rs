use crate::secrets;
use crate::{
    store::{Profile, SessionOptions},
    ui::State,
};
use adw::prelude::*;
use std::{cell::Cell, rc::Rc};
use zeroize::Zeroizing;

fn folder_header(state: &Rc<State>, folder: &str) -> gtk::ListBoxRow {
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let title = if folder.is_empty() {
        tr!("Без папки", "Ungrouped")
    } else {
        folder
    };
    let collapsed = state.collapsed_folders.borrow().contains(folder);
    let toggle = gtk::Button::from_icon_name(if collapsed {
        "pan-end-symbolic"
    } else {
        "pan-down-symbolic"
    });
    toggle.set_tooltip_text(Some(if collapsed {
        tr!("Развернуть папку", "Expand Folder")
    } else {
        tr!("Свернуть папку", "Collapse Folder")
    }));
    toggle.add_css_class("flat");
    let toggle_state = state.clone();
    let toggle_folder = folder.to_owned();
    toggle.connect_clicked(move |_| {
        let mut collapsed = toggle_state.collapsed_folders.borrow_mut();
        if !collapsed.remove(&toggle_folder) {
            collapsed.insert(toggle_folder.clone());
        }
        drop(collapsed);
        refresh(&toggle_state);
    });
    heading.append(&toggle);
    let title = gtk::Label::builder()
        .label(title)
        .hexpand(true)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    title.add_css_class("heading");
    heading.append(&title);
    if !folder.is_empty() {
        let rename = gtk::Button::from_icon_name("document-edit-symbolic");
        rename.set_tooltip_text(Some(tr!("Переименовать папку", "Rename Folder")));
        rename.add_css_class("flat");
        let state = state.clone();
        let folder = folder.to_owned();
        rename.connect_clicked(move |button| {
            let Some(window) = button.root().and_downcast::<gtk::Window>() else {
                return;
            };
            rename_folder(&window, state.clone(), folder.clone());
        });
        heading.append(&rename);
    }
    gtk::ListBoxRow::builder()
        .selectable(false)
        .activatable(false)
        .child(&heading)
        .build()
}

fn rename_folder(window: &gtk::Window, state: Rc<State>, old: String) {
    let dialog = adw::AlertDialog::builder()
        .heading(tr!("Переименовать папку", "Rename Folder"))
        .build();
    let entry = gtk::Entry::builder()
        .text(&old)
        .max_length(80)
        .activates_default(true)
        .build();
    dialog.set_extra_child(Some(&entry));
    dialog.add_responses(&[
        ("cancel", tr!("Отмена", "Cancel")),
        ("rename", tr!("Переименовать", "Rename")),
    ]);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("rename"));
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
    let weak_window = window.downgrade();
    dialog.connect_response(None, move |_, response| {
        if response != "rename" {
            return;
        }
        let value = entry.text();
        let new = value.trim();
        match state.store.rename_folder(&old, new) {
            Ok(()) => {
                let mut collapsed = state.collapsed_folders.borrow_mut();
                if collapsed.remove(&old) {
                    collapsed.insert(new.to_owned());
                }
                drop(collapsed);
                refresh(&state);
            }
            Err(error) => {
                if let Some(window) = weak_window.upgrade() {
                    let alert = adw::AlertDialog::builder()
                        .heading(tr!(
                            "Не удалось переименовать папку",
                            "Could not rename the folder"
                        ))
                        .body(error.to_string())
                        .build();
                    alert.add_response("close", tr!("Закрыть", "Close"));
                    alert.present(Some(&window));
                }
            }
        }
    });
    dialog.present(Some(window));
}

pub(crate) fn sort_profiles(
    profiles: &mut [&Profile],
    folders: &std::collections::HashMap<String, String>,
    counts: &std::collections::HashMap<String, u64>,
    frequently: bool,
) {
    profiles.sort_by_cached_key(|profile| {
        let count = if frequently {
            counts.get(&profile.uid).copied().unwrap_or(0)
        } else {
            0
        };
        (
            folders.get(&profile.uid),
            std::cmp::Reverse(count),
            profile.name.to_lowercase(),
        )
    });
}

pub fn refresh(state: &Rc<State>) {
    state
        .lists
        .borrow_mut()
        .retain(|view| view.list.upgrade().is_some());
    let folders = state.store.profile_folders().unwrap_or_default();
    let profiles = state.store.profiles();
    let counts = state.store.connection_counts().unwrap_or_default();
    for view in state.lists.borrow().iter() {
        let Some(list) = view.list.upgrade() else {
            continue;
        };
        let frequently = view.frequently();
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        match &profiles {
            Err(error) => {
                let label = gtk::Label::new(Some(&tr_format!(
                    "Ошибка чтения профилей: {error}",
                    "Could not load profiles: {error}"
                )));
                label.set_wrap(true);
                list.append(&label);
            }
            Ok(profiles) if profiles.is_empty() => {
                list.append(&gtk::Label::new(Some(tr!(
                    "Нет сохранённых профилей",
                    "No saved profiles"
                ))));
            }
            Ok(profiles) => {
                let mut profiles: Vec<_> = profiles.iter().collect();
                sort_profiles(&mut profiles, &folders, &counts, frequently);
                let mut previous = None;
                for profile in profiles {
                    let folder = folders.get(&profile.uid).map(String::as_str).unwrap_or("");
                    if previous != Some(folder) {
                        list.append(&folder_header(state, folder));
                        previous = Some(folder);
                    }
                    let row = adw::ActionRow::builder()
                        .title(&profile.name)
                        .subtitle(tr_format!(
                            "{} · порт {}",
                            "{} · port {}",
                            profile.host,
                            profile.port
                        ))
                        .title_lines(1)
                        .subtitle_lines(1)
                        .activatable(true)
                        .build();
                    row.set_use_markup(false);
                    row.set_widget_name(
                        folders.get(&profile.uid).map(String::as_str).unwrap_or(""),
                    );

                    row.set_tooltip_text(Some(tr!(
                        "Открыть настройки профиля",
                        "Open profile settings"
                    )));
                    let connect = gtk::Button::builder()
                        .icon_name("media-playback-start-symbolic")
                        .tooltip_text(tr!("Подключиться", "Connect"))
                        .valign(gtk::Align::Center)
                        .build();
                    row.add_suffix(&connect);
                    let more = gtk::MenuButton::builder()
                        .icon_name("view-more-symbolic")
                        .valign(gtk::Align::Center)
                        .tooltip_text(tr!("Действия с подключением", "Connection Actions"))
                        .build();
                    let popover = gtk::Popover::new();
                    let clone_button = gtk::Button::with_label(tr!("Клонировать…", "Duplicate…"));
                    popover.set_child(Some(&clone_button));
                    more.set_popover(Some(&popover));
                    row.add_suffix(&more);
                    let weak_popover = popover.downgrade();
                    let clone_state = state.clone();
                    let source = profile.clone();
                    clone_button.connect_clicked(move |button| {
                        if let Some(window) = button.root().and_downcast::<adw::ApplicationWindow>()
                        {
                            if let Some(popover) = weak_popover.upgrade() {
                                popover.popdown();
                            }
                            let uid = source.uid.clone();
                            let mut draft = source.clone();
                            draft.id = None;
                            draft.uid = gtk::glib::uuid_string_random().into();
                            let profiles = clone_state.store.profiles().unwrap_or_default();
                            draft.name = tr_format!("{} (копия)", "{} (copy)", source.name);
                            for index in 2..=profiles.len() + 2 {
                                if !profiles.iter().any(|p| p.name == draft.name) {
                                    break;
                                }
                                draft.name = tr_format!(
                                    "{} (копия {index})",
                                    "{} (copy {index})",
                                    source.name
                                );
                            }
                            edit_draft(&window, clone_state.clone(), draft, Some(uid));
                        }
                    });

                    let connection_state = state.clone();
                    let connection_profile = profile.clone();
                    connect.connect_clicked(move |button| {
                        if let Some(window) = button.root().and_downcast::<adw::ApplicationWindow>()
                        {
                            if let Some(tabs) = connection_state.tabs_for(&window) {
                                crate::rdp_ui::connect(
                                    &window,
                                    &tabs,
                                    connection_state.clone(),
                                    connection_profile.clone(),
                                );
                            }
                        }
                    });
                    let state = state.clone();
                    let profile = profile.clone();
                    row.connect_activated(move |row| {
                        if let Some(window) = row.root().and_downcast::<adw::ApplicationWindow>() {
                            edit(&window, state.clone(), profile.clone());
                        }
                    });
                    list.append(&row);
                }
            }
        }
        list.invalidate_filter();
    }
}

pub fn edit(parent: &adw::ApplicationWindow, state: Rc<State>, profile: Profile) {
    edit_draft(parent, state, profile, None);
}
fn edit_draft(
    parent: &adw::ApplicationWindow,
    state: Rc<State>,
    profile: Profile,
    clone_source: Option<String>,
) {
    let options = match state
        .store
        .session_options(clone_source.as_deref().unwrap_or(&profile.uid))
    {
        Ok(options) => options,
        Err(error) => {
            let alert = adw::AlertDialog::builder()
                .heading(tr!(
                    "Не удалось открыть настройки",
                    "Could not open settings"
                ))
                .body(error.to_string())
                .build();
            alert.add_response("close", tr!("Закрыть", "Close"));
            alert.present(Some(parent));
            return;
        }
    };

    let dialog = adw::Dialog::builder()
        .title(if profile.id.is_some() {
            tr!("Профиль подключения", "Connection Profile")
        } else {
            tr!("Новое подключение", "New Connection")
        })
        .content_width(520)
        .content_height(620)
        .build();
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
    layout.append(&adw::HeaderBar::new());
    let form = gtk::Box::new(gtk::Orientation::Vertical, 12);
    form.set_margin_top(12);
    form.set_margin_bottom(12);
    form.set_margin_start(12);
    form.set_margin_end(12);
    let group = adw::PreferencesGroup::new();
    let entry = |title: &str, value: &str| {
        let row = adw::EntryRow::builder().title(title).text(value).build();
        group.add(&row);
        row
    };
    let name = entry(tr!("Название", "Name"), &profile.name);
    let host = entry(
        tr!("Сервер (DNS или IP)", "Server (DNS or IP)"),
        &profile.host,
    );
    let port = entry(tr!("Порт", "Port"), &profile.port.to_string());
    let username = entry(tr!("Пользователь", "Username"), &profile.username);
    let domain = entry(
        tr!("Домен (необязательно)", "Domain (optional)"),
        &profile.domain,
    );
    let folders = state.store.profile_folders().unwrap_or_default();
    let folder_key = clone_source.as_deref().unwrap_or(&profile.uid);
    let initial_folder = folders.get(folder_key).cloned().unwrap_or_default();
    let folder = entry(
        tr!("Папка (необязательно)", "Folder (optional)"),
        &initial_folder,
    );
    let new_profile = profile.id.is_none();
    let known: std::collections::BTreeSet<_> = folders.values().map(String::as_str).collect();
    if !known.is_empty() {
        let choices = gtk::MenuButton::builder()
            .icon_name("folder-open-symbolic")
            .tooltip_text(tr!(
                "Выбрать существующую папку",
                "Choose an existing folder"
            ))
            .valign(gtk::Align::Center)
            .build();
        let popover = gtk::Popover::new();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_width_request(310);
        let search = gtk::SearchEntry::builder()
            .placeholder_text(tr!("Найти папку", "Find a folder"))
            .build();
        content.append(&search);
        let items = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let mut filtered = Vec::new();
        for name in known {
            let title = gtk::Label::builder()
                .label(name)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .xalign(0.0)
                .build();
            let button = gtk::Button::builder().child(&title).build();
            button.add_css_class("flat");
            filtered.push((button.downgrade(), name.to_lowercase()));
            let folder = folder.downgrade();
            let popover = popover.downgrade();
            let name = name.to_owned();
            button.connect_clicked(move |_| {
                if let Some(folder) = folder.upgrade() {
                    folder.set_text(&name);
                }
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
            });
            items.append(&button);
        }
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(360)
            .child(&items)
            .build();
        content.append(&scroll);
        search.connect_search_changed(move |search| {
            let query = search.text().trim().to_lowercase();
            for (button, name) in &filtered {
                if let Some(button) = button.upgrade() {
                    button.set_visible(name.contains(&query));
                }
            }
        });
        popover.set_child(Some(&content));
        choices.set_popover(Some(&popover));
        folder.add_suffix(&choices);
    }

    form.append(&group);
    let advanced = adw::ExpanderRow::builder()
        .title(tr!("Дополнительные настройки", "Advanced Settings"))
        .build();
    let combo = |title: &str, items: &[&str], selected: u32| {
        let row = adw::ComboRow::builder()
            .title(title)
            .model(&gtk::StringList::new(items))
            .selected(selected)
            .build();
        advanced.add_row(&row);
        row
    };
    let keyboard_layout = combo(
        tr!("Раскладка RDP", "RDP Keyboard Layout"),
        &[
            tr!("Русская (ЙЦУКЕН)", "Russian (ЙЦУКЕН)"),
            tr!("Английская (US)", "English (US)"),
        ],
        u32::from(options.keyboard_layout != 0x419),
    );
    let quality = combo(
        tr!("Графика", "Graphics"),
        &[
            tr!("Авто: скорость и качество", "Automatic: speed and quality"),
            tr!("Приоритет чёткости", "Prioritize sharpness"),
            tr!(
                "H.264 без GPU (совместимость)",
                "H.264 without GPU (compatibility)"
            ),
            tr!("Точная передача (без потерь)", "Lossless"),
            tr!(
                "AVC444: скорость и чёткий текст",
                "AVC444: fast with sharp text"
            ),
        ],
        options.quality,
    );
    let resolution = combo(
        tr!("Разрешение", "Resolution"),
        &[
            tr!(
                "Автоматически — по размеру окна",
                "Automatic — match window size"
            ),
            "1280 × 720",
            "1600 × 900",
            "1920 × 1080",
        ],
        options.resolution,
    );
    let shared_folder = adw::EntryRow::builder()
        .title(tr!(
            "Общая папка на этом ПК (необязательно)",
            "Shared folder on this computer (optional)"
        ))
        .text(&options.shared_folder)
        .build();
    let browse_folder = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text(tr!("Выбрать папку", "Choose Folder"))
        .valign(gtk::Align::Center)
        .build();
    shared_folder.add_suffix(&browse_folder);
    let clear_folder = gtk::Button::builder()
        .icon_name("edit-clear-symbolic")
        .tooltip_text(tr!("Отключить общую папку", "Disable Shared Folder"))
        .valign(gtk::Align::Center)
        .build();
    shared_folder.add_suffix(&clear_folder);
    let folder_to_clear = shared_folder.downgrade();
    clear_folder.connect_clicked(move |_| {
        if let Some(folder) = folder_to_clear.upgrade() {
            folder.set_text("");
        }
    });
    let weak_window = parent.downgrade();
    let folder_to_choose = shared_folder.downgrade();
    browse_folder.connect_clicked(move |_| {
        let (Some(window), Some(folder)) = (weak_window.upgrade(), folder_to_choose.upgrade())
        else {
            return;
        };
        gtk::glib::MainContext::default().spawn_local(async move {
            let chooser = gtk::FileDialog::new();
            chooser.set_title(tr!(
                "Папка для передачи файлов по RDP",
                "Folder for RDP file transfer"
            ));
            if let Ok(file) = chooser.select_folder_future(Some(&window)).await {
                if let Some(path) = file.path() {
                    folder.set_text(&path.to_string_lossy());
                }
            }
        });
    });
    advanced.add_row(&shared_folder);
    let advanced_group = adw::PreferencesGroup::new();
    advanced_group.add(&advanced);
    form.append(&advanced_group);

    let password = adw::PasswordEntryRow::builder()
        .title(tr!(
            "Новый пароль (только на этом ПК)",
            "New password (this computer only)"
        ))
        .build();
    let credentials = adw::PreferencesGroup::builder()
        .description(tr!("Пустое поле оставляет сохранённый пароль без изменений. Пароли не входят в импорт и будущую синхронизацию.", "Leave blank to keep the saved password. Profile imports and sync exclude passwords."))
        .build();
    credentials.add(&password);
    form.append(&credentials);
    let clear_password = gtk::CheckButton::with_label(tr!(
        "Удалить сохранённый пароль при сохранении",
        "Remove the saved password on save"
    ));
    form.append(&clear_password);
    let check_password = gtk::Button::with_label(tr!(
        "Проверить наличие сохранённого пароля",
        "Check for a saved password"
    ));
    form.append(&check_password);
    // Keep actions and status visible while the taller form can scroll.
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    footer.set_margin_top(8);
    footer.set_margin_bottom(12);
    footer.set_margin_start(12);
    footer.set_margin_end(12);
    let error = gtk::Label::new(None);
    error.set_wrap(true);
    error.set_visible(false);
    footer.append(&error);
    let save = gtk::Button::with_label(tr!("Сохранить", "Save"));
    save.add_css_class("suggested-action");
    footer.append(&save);
    let current_id = Rc::new(Cell::new(profile.id));
    let uid = profile.uid.clone();
    let check_uid = uid.clone();
    let check_state = state.clone();
    let weak_dialog = dialog.downgrade();
    let check_form = layout.downgrade();
    let check_error = error.clone();
    check_password.connect_clicked(move |_| {
        let Some(dialog) = weak_dialog.upgrade() else {
            return;
        };
        let Some(operation) = check_state.operation(&check_uid) else {
            show_error(&check_error, tr!("В другом окне выполняется операция с этим профилем.", "This profile is being updated in another window."));
            check_error.set_visible(true);
            return;
        };
        dialog.set_can_close(false);
        let Some(form) = check_form.upgrade() else { return; };
        form.set_sensitive(false);
        let error = check_error.clone();
        let uid = check_uid.clone();
        gtk::glib::MainContext::default().spawn_local(async move {
            let result = secrets::exists(&uid).await;
            drop(operation);
            match result {
                Ok(true) => show_status(&error, tr!("Пароль сохранён на этом ПК. Поле нового пароля остаётся пустым — это нормально.", "This computer has a saved password. The new password field stays blank."), "success"),
                Ok(false) => show_status(&error, tr!("Сохранённого пароля на этом ПК нет.", "No saved password on this computer."), "dim-label"),
                Err(message) => show_error(&error, message),
            }
            error.set_visible(true);
            form.set_sensitive(true);
            dialog.set_can_close(true);
        });
    });
    let weak_dialog = dialog.downgrade();
    let save_state = state.clone();
    let save_error = error.clone();
    let save_form = layout.downgrade();
    let save_id = current_id.clone();
    let clone_password = password.downgrade();
    save.connect_clicked(move |_| {
        let Some(dialog) = weak_dialog.upgrade() else { return; };
        let Some(operation) = save_state.operation(&uid) else {
            show_error(&save_error, tr!("В другом окне выполняется операция с этим профилем.", "This profile is being updated in another window."));
            save_error.set_visible(true);
            return;
        };
        if clear_password.is_active() && !password.text().is_empty() {
            show_error(&save_error, tr!("Выберите одно действие: ввести новый пароль или удалить сохранённый.", "Enter a new password or remove the saved one."));
            save_error.set_visible(true);
            return;
        }
        let result = (|| {
            let port = port.text().trim().parse::<u16>()
                .map_err(|_| tr!("Порт должен быть от 1 до 65535.", "Enter a port from 1 to 65535."))?;
            let folder_text = folder.text();
            let selected_folder = folder_text.trim();
            let folder_update = (new_profile || selected_folder != initial_folder).then_some(selected_folder);
            save_state.store.save_with_folder(&Profile {
                id: save_id.get(),
                uid: uid.clone(),
                name: name.text().trim().into(),
                host: host.text().trim().into(),
                port,
                username: username.text().trim().into(),
                domain: domain.text().trim().into(),
            }, &SessionOptions { keyboard_layout:if keyboard_layout.selected()==0 {0x419}else{0x409},
                quality:quality.selected(), resolution:resolution.selected(), shared_folder:shared_folder.text().trim().into() }, folder_update)
        })();
        let id = match result {
            Ok(id) => id,
            Err(message) => {
                show_error(&save_error, &message.to_string());
                save_error.set_visible(true);
                return;
            }
        };
        save_id.set(Some(id));
        refresh(&save_state);
        let value = Zeroizing::new(password.text().to_string());
        let clear = clear_password.is_active();
        if value.is_empty() && !clear {
            dialog.close();
            return;
        }
        dialog.set_can_close(false);
        let Some(form) = save_form.upgrade() else { return; };
        form.set_sensitive(false);
        let error = save_error.clone();
        let password = password.clone();
        let uid = uid.clone();
        gtk::glib::MainContext::default().spawn_local(async move {
            let result = if clear { secrets::clear(&uid).await } else { secrets::store(&uid, &value).await };
            drop(value);
            drop(operation);
            form.set_sensitive(true);
            dialog.set_can_close(true);
            match result {
                Ok(()) => {
                    password.set_text("");
                    dialog.close();
                }
                Err(message) => {
                    show_error(&error, &tr_format!("Параметры профиля сохранены. Операция с паролем не выполнена. {message} Повторите сохранение после разблокировки хранилища.", "Profile settings were saved. The password operation failed. {message} Unlock the keyring and try saving again."));
                    error.set_visible(true);
                }
            }
        });
    });
    if let Some(id) = profile.id {
        let delete = gtk::Button::with_label(tr!("Удалить профиль…", "Delete Profile…"));
        delete.add_css_class("destructive-action");
        footer.append(&delete);
        let weak_dialog = dialog.downgrade();
        let weak_form = layout.downgrade();
        let delete_error = error.clone();
        delete.connect_clicked(move |button| {
            let confirm = adw::AlertDialog::builder()
                .heading(tr!("Удалить профиль и локальный пароль?", "Delete the profile and its saved password?"))
                .body(tr!("Сохранённые параметры и пароль этого подключения на этом ПК будут удалены. Это действие нельзя отменить.", "This removes the saved settings and password from this computer."))
                .build();
            confirm.add_responses(&[("cancel", tr!("Отмена", "Cancel")), ("delete", tr!("Удалить", "Delete"))]);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");
            confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            let state = state.clone();
            let weak_dialog = weak_dialog.clone();
            let weak_form = weak_form.clone();
            let error = delete_error.clone();
            let uid = profile.uid.clone();
            confirm.connect_response(None, move |_, response| {
                if response != "delete" { return; }
                let (Some(dialog), Some(form)) = (weak_dialog.upgrade(), weak_form.upgrade()) else { return; };
                let Some(operation) = state.operation(&uid) else {
                    show_error(&error, tr!("В другом окне выполняется операция с этим профилем.", "This profile is being updated in another window."));
                    error.set_visible(true);
                    return;
                };
                dialog.set_can_close(false);
                form.set_sensitive(false);
                let state = state.clone();
                let uid = uid.clone();
                let error = error.clone();
                gtk::glib::MainContext::default().spawn_local(async move {
                    let result = match secrets::clear(&uid).await {
                        Ok(()) => state.store.delete(id),
                        Err(message) => Err(message.into()),
                    };
                    drop(operation);
                    form.set_sensitive(true);
                    dialog.set_can_close(true);
                    match result {
                        Ok(()) => { refresh(&state); dialog.close(); }
                        Err(message) => {
                            show_error(&error, &tr_format!("Профиль не удалён. {message}", "Profile deletion failed. {message}"));
                            error.set_visible(true);
                        }
                    }
                });
            });
            confirm.present(Some(button));
        });
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&form)
        .build();
    layout.append(&scroll);
    layout.append(&footer);
    dialog.set_child(Some(&layout));
    if let Some(source_uid) = clone_source {
        layout.set_sensitive(false);
        let weak_layout = layout.downgrade();
        let weak_dialog = dialog.downgrade();
        let weak_password = clone_password;
        let error = error.clone();
        gtk::glib::MainContext::default().spawn_local(async move {
            let result = secrets::lookup_rdp(&source_uid).await;
            if let (Some(layout), Some(_dialog), Some(password)) = (
                weak_layout.upgrade(),
                weak_dialog.upgrade(),
                weak_password.upgrade(),
            ) {
                match result {
                    Ok(Some(secret)) => password.set_text(&secret),
                    Ok(None) => {}
                    Err(message) => {
                        show_error(
                            &error,
                            &tr_format!(
                                "Пароль не скопирован. {message}",
                                "Password copy failed. {message}"
                            ),
                        );
                        error.set_visible(true);
                    }
                }
                layout.set_sensitive(true);
            }
        });
    }
    dialog.present(Some(parent));
}

fn show_status(label: &gtk::Label, message: &str, class: &str) {
    for previous in ["success", "error", "dim-label"] {
        label.remove_css_class(previous);
    }
    label.add_css_class(class);
    label.set_text(message);
    label.set_visible(true);
}

fn show_error(label: &gtk::Label, message: &str) {
    show_status(label, message, "error");
}
