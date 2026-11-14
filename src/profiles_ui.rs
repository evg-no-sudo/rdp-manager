use crate::secrets;
use crate::{store::Profile, ui::State};
use adw::prelude::*;
use std::{cell::Cell, rc::Rc};
use zeroize::Zeroizing;

pub fn refresh(state: &Rc<State>) {
    state
        .lists
        .borrow_mut()
        .retain(|weak| weak.upgrade().is_some());
    let profiles = state.store.profiles();
    for list in state
        .lists
        .borrow()
        .iter()
        .filter_map(|weak| weak.upgrade())
    {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
        match &profiles {
            Err(error) => {
                let label = gtk::Label::new(Some(&format!("Ошибка чтения профилей: {error}")));
                label.set_wrap(true);
                list.append(&label);
            }
            Ok(profiles) if profiles.is_empty() => {
                list.append(&gtk::Label::new(Some("Нет сохранённых профилей")));
            }
            Ok(profiles) => {
                for profile in profiles {
                    let row = adw::ActionRow::builder()
                        .title(&profile.name)
                        .subtitle(format!("{} · порт {}", profile.host, profile.port))
                        .activatable(true)
                        .build();
                    row.set_use_markup(false);
                    row.set_tooltip_text(Some("Открыть настройки профиля"));
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
    }
}

pub fn edit(parent: &adw::ApplicationWindow, state: Rc<State>, profile: Profile) {
    let dialog = adw::Dialog::builder()
        .title(if profile.id.is_some() {
            "Профиль подключения"
        } else {
            "Новый профиль"
        })
        .content_width(500)
        .build();
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 12);
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
    let name = entry("Название", &profile.name);
    let host = entry("Сервер (DNS или IP)", &profile.host);
    let port = entry("Порт", &profile.port.to_string());
    let username = entry("Пользователь", &profile.username);
    let domain = entry("Домен (необязательно)", &profile.domain);
    form.append(&group);
    let password = adw::PasswordEntryRow::builder()
        .title("Новый пароль (только на этом ПК)")
        .build();
    let credentials = adw::PreferencesGroup::builder()
        .description("Пустое поле оставляет сохранённый пароль без изменений. Пароли не входят в импорт и будущую синхронизацию.")
        .build();
    credentials.add(&password);
    form.append(&credentials);
    let clear_password = gtk::CheckButton::with_label("Удалить сохранённый пароль при сохранении");
    form.append(&clear_password);
    let check_password = gtk::Button::with_label("Проверить наличие сохранённого пароля");
    form.append(&check_password);
    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    error.set_visible(false);
    form.append(&error);
    let save = gtk::Button::with_label("Сохранить");
    save.add_css_class("suggested-action");
    form.append(&save);
    let current_id = Rc::new(Cell::new(profile.id));
    let uid = profile.uid.clone();
    let check_uid = uid.clone();
    let check_state = state.clone();
    let weak_dialog = dialog.downgrade();
    let check_form = form.clone();
    let check_error = error.clone();
    check_password.connect_clicked(move |_| {
        let Some(dialog) = weak_dialog.upgrade() else {
            return;
        };
        let Some(operation) = check_state.operation(&check_uid) else {
            check_error.set_text("В другом окне выполняется операция с этим профилем.");
            check_error.set_visible(true);
            return;
        };
        dialog.set_can_close(false);
        check_form.set_sensitive(false);
        let form = check_form.clone();
        let error = check_error.clone();
        let uid = check_uid.clone();
        gtk::glib::MainContext::default().spawn_local(async move {
            let result = secrets::exists(&uid).await;
            drop(operation);
            error.set_text(match result {
                Ok(true) => "Пароль найден в локальном Secret Service.",
                Ok(false) => "Сохранённого пароля на этом ПК нет.",
                Err(message) => message,
            });
            error.set_visible(true);
            form.set_sensitive(true);
            dialog.set_can_close(true);
        });
    });
    let weak_dialog = dialog.downgrade();
    let save_state = state.clone();
    let save_error = error.clone();
    let save_form = form.clone();
    let save_id = current_id.clone();
    save.connect_clicked(move |_| {
        let Some(dialog) = weak_dialog.upgrade() else { return; };
        let Some(operation) = save_state.operation(&uid) else {
            save_error.set_text("В другом окне выполняется операция с этим профилем.");
            save_error.set_visible(true);
            return;
        };
        if clear_password.is_active() && !password.text().is_empty() {
            save_error.set_text("Выберите одно действие: ввести новый пароль или удалить сохранённый.");
            save_error.set_visible(true);
            return;
        }
        let result = (|| {
            let port = port.text().trim().parse::<u16>()
                .map_err(|_| "Порт должен быть от 1 до 65535.")?;
            save_state.store.save(&Profile {
                id: save_id.get(),
                uid: uid.clone(),
                name: name.text().trim().into(),
                host: host.text().trim().into(),
                port,
                username: username.text().trim().into(),
                domain: domain.text().trim().into(),
            })
        })();
        let id = match result {
            Ok(id) => id,
            Err(message) => {
                save_error.set_text(&message.to_string());
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
        save_form.set_sensitive(false);
        let form = save_form.clone();
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
                    error.set_text(&format!("Параметры профиля сохранены. Операция с паролем не выполнена. {message} Повторите сохранение после разблокировки хранилища."));
                    error.set_visible(true);
                }
            }
        });
    });
    if let Some(id) = profile.id {
        let delete = gtk::Button::with_label("Удалить профиль…");
        delete.add_css_class("destructive-action");
        form.append(&delete);
        let weak_dialog = dialog.downgrade();
        let weak_form = form.downgrade();
        delete.connect_clicked(move |button| {
            let confirm = adw::AlertDialog::builder()
                .heading("Удалить профиль и локальный пароль?")
                .body("Сохранённые параметры и пароль этого подключения на этом ПК будут удалены. Это действие нельзя отменить.")
                .build();
            confirm.add_responses(&[("cancel", "Отмена"), ("delete", "Удалить")]);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");
            confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            let state = state.clone();
            let weak_dialog = weak_dialog.clone();
            let weak_form = weak_form.clone();
            let error = error.clone();
            let uid = profile.uid.clone();
            confirm.connect_response(None, move |_, response| {
                if response != "delete" { return; }
                let (Some(dialog), Some(form)) = (weak_dialog.upgrade(), weak_form.upgrade()) else { return; };
                let Some(operation) = state.operation(&uid) else {
                    error.set_text("В другом окне выполняется операция с этим профилем.");
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
                            error.set_text(&format!("Профиль не удалён. {message}"));
                            error.set_visible(true);
                        }
                    }
                });
            });
            confirm.present(Some(button));
        });
    }
    layout.append(&form);
    dialog.set_child(Some(&layout));
    dialog.present(Some(parent));
}
