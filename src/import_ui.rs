use crate::{
    profiles_ui, remmina,
    store::{Profile, Result},
    ui::State,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

pub fn choose(parent: &adw::ApplicationWindow, state: Rc<State>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Профили Remmina (*.remmina)"));
    filter.add_pattern("*.remmina");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let chooser = gtk::FileDialog::builder()
        .title("Импорт профилей Remmina")
        .modal(true)
        .filters(&filters)
        .default_filter(&filter)
        .build();
    let weak_parent = parent.downgrade();
    let future = chooser.open_multiple_future(Some(parent));
    glib::MainContext::default().spawn_local(async move {
        let files = match future.await {
            Ok(files) => files,
            Err(error) => {
                if !error.matches(gtk::DialogError::Dismissed) {
                    if let Some(parent) = weak_parent.upgrade() {
                        preview(
                            &parent,
                            state,
                            vec![(
                                "Выбор файлов".into(),
                                Err("Не удалось выбрать файлы.".into()),
                            )],
                        );
                    }
                }
                return;
            }
        };
        if files.n_items() > 200 {
            if let Some(parent) = weak_parent.upgrade() {
                preview(
                    &parent,
                    state,
                    vec![(
                        "Выбор файлов".into(),
                        Err("Выберите не более 200 файлов за один импорт.".into()),
                    )],
                );
            }
            return;
        }
        let mut entries = Vec::new();
        for index in 0..files.n_items() {
            if let Some(file) = files.item(index).and_downcast::<gio::File>() {
                let name = file
                    .basename()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Файл Remmina".into());
                entries.push((name, read_profile(&file).await));
            }
        }
        if let Some(parent) = weak_parent.upgrade() {
            preview(&parent, state, entries);
        }
    });
}

async fn read_profile(file: &gio::File) -> Result<Profile> {
    let stream = file
        .read_future(glib::Priority::DEFAULT)
        .await
        .map_err(|_| "Не удалось открыть файл.")?;
    let result = async {
        let mut data = Vec::new();
        loop {
            let bytes = stream
                .read_bytes_future(65536, glib::Priority::DEFAULT)
                .await
                .map_err(|_| "Не удалось прочитать файл.")?;
            if bytes.is_empty() {
                break;
            }
            if data.len() + bytes.len() > 1024 * 1024 {
                return Err("Файл превышает 1 МиБ.".into());
            }
            data.extend_from_slice(&bytes);
        }
        let text = std::str::from_utf8(&data).map_err(|_| "Файл должен быть в UTF-8.")?;
        remmina::parse(text)
    }
    .await;
    let _ = stream.close_future(glib::Priority::DEFAULT).await;
    result
}

fn preview(
    parent: &adw::ApplicationWindow,
    state: Rc<State>,
    entries: Vec<(String, Result<Profile>)>,
) {
    let dialog = adw::Dialog::builder()
        .title("Импорт Remmina — просмотр")
        .content_width(620)
        .content_height(560)
        .build();
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 12);
    layout.append(&adw::HeaderBar::new());
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(16);
    content.set_margin_end(16);
    content.set_margin_bottom(16);
    let notice = gtk::Label::new(Some("Переносятся только название, сервер, порт, пользователь и домен. Пароли, шлюзы, туннели, команды и остальные настройки не переносятся. Одинаковые профили будут пропущены. Снимите отметки с ненужных профилей."));
    notice.set_wrap(true);
    notice.set_xalign(0.0);
    content.append(&notice);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
    let mut candidates = Vec::new();
    for (filename, result) in entries {
        let row = adw::ActionRow::new();
        row.set_use_markup(false);
        match result {
            Ok(profile) => {
                row.set_title(&profile.name);
                row.set_subtitle(&format!(
                    "{} · порт {} · пользователь: {} · домен: {}\n{}",
                    profile.host, profile.port, profile.username, profile.domain, filename
                ));
                let check = gtk::CheckButton::builder()
                    .active(true)
                    .valign(gtk::Align::Center)
                    .build();
                row.add_prefix(&check);
                row.set_activatable_widget(Some(&check));
                candidates.push((check, profile));
            }
            Err(error) => {
                row.set_title(&filename);
                row.set_subtitle(&error.to_string());
                row.add_css_class("error");
            }
        }
        list.append(&row);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    content.append(&scroll);
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    content.append(&status);
    let import = gtk::Button::with_label("Импортировать отмеченные");
    import.add_css_class("suggested-action");
    import.set_sensitive(!candidates.is_empty());
    content.append(&import);
    import.connect_clicked(move |button| {
        let profiles: Vec<_> = candidates.iter().filter(|(check, _)| check.is_active())
            .map(|(_, profile)| profile.clone()).collect();
        if profiles.is_empty() {
            status.set_text("Отметьте хотя бы один профиль.");
            return;
        }
        match state.store.import(&profiles) {
            Ok(added) => {
                profiles_ui::refresh(&state);
                status.set_text(&format!("Добавлено: {added}. Дубликатов пропущено: {}.", profiles.len() - added));
                button.set_sensitive(false);
                for (check, _) in &candidates { check.set_sensitive(false); }
            }
            Err(_) => {
                status.set_text("Импорт не выполнен. База данных недоступна или произошла ошибка записи; изменения отменены.");
            }
        }
    });
    layout.append(&content);
    dialog.set_child(Some(&layout));
    dialog.present(Some(parent));
}
