use crate::{store::Profile, ui::State};
use adw::prelude::*;
use std::rc::Rc;

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
    let error = gtk::Label::new(None);
    error.add_css_class("error");
    error.set_wrap(true);
    error.set_visible(false);
    form.append(&error);
    let save = gtk::Button::with_label("Сохранить");
    save.add_css_class("suggested-action");
    form.append(&save);
    let weak_dialog = dialog.downgrade();
    let save_state = state.clone();
    let save_error = error.clone();
    let id = profile.id;
    save.connect_clicked(move |_| {
        let result = (|| {
            let port = port
                .text()
                .trim()
                .parse::<u16>()
                .map_err(|_| "Порт должен быть от 1 до 65535.")?;
            save_state.store.save(&Profile {
                id,
                name: name.text().trim().into(),
                host: host.text().trim().into(),
                port,
                username: username.text().trim().into(),
                domain: domain.text().trim().into(),
            })
        })();
        match result {
            Ok(()) => {
                refresh(&save_state);
                if let Some(dialog) = weak_dialog.upgrade() {
                    dialog.close();
                }
            }
            Err(message) => {
                save_error.set_text(&message.to_string());
                save_error.set_visible(true);
            }
        }
    });
    if let Some(id) = profile.id {
        let delete = gtk::Button::with_label("Удалить профиль…");
        delete.add_css_class("destructive-action");
        form.append(&delete);
        let weak_dialog = dialog.downgrade();
        delete.connect_clicked(move |button| {
            let confirm = adw::AlertDialog::builder()
                .heading("Удалить профиль?")
                .body("Сохранённые параметры будут удалены. Это действие нельзя отменить.")
                .build();
            confirm.add_responses(&[("cancel", "Отмена"), ("delete", "Удалить")]);
            confirm.set_default_response(Some("cancel"));
            confirm.set_close_response("cancel");
            confirm.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            let state = state.clone();
            let weak_dialog = weak_dialog.clone();
            let error = error.clone();
            confirm.connect_response(None, move |_, response| {
                if response == "delete" {
                    match state.store.delete(id) {
                        Ok(()) => {
                            refresh(&state);
                            if let Some(dialog) = weak_dialog.upgrade() {
                                dialog.close();
                            }
                        }
                        Err(message) => {
                            error.set_text(&message.to_string());
                            error.set_visible(true);
                        }
                    }
                }
            });
            confirm.present(Some(button));
        });
    }
    layout.append(&form);
    dialog.set_child(Some(&layout));
    dialog.present(Some(parent));
}
