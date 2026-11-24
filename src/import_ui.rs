use crate::{
    profiles_ui, remmina,
    store::{Profile, Result},
    ui::State,
};
use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

pub fn discover(parent: &adw::ApplicationWindow, state: Rc<State>) {
    let home = glib::home_dir();
    let directories = [
        glib::user_data_dir().join("remmina"),
        home.join(".remmina"),
        home.join(".var/app/org.remmina.Remmina/data/remmina"),
    ]
    .into_iter()
    .filter(|p| p.is_dir())
    .map(gio::File::for_path)
    .collect();
    scan_and_preview(parent, state, directories);
}

pub fn choose_directory(parent: &adw::ApplicationWindow, state: Rc<State>) {
    let chooser = gtk::FileDialog::builder()
        .title(tr!(
            "Каталог подключений Remmina",
            "Remmina Profiles Folder"
        ))
        .modal(true)
        .build();
    let future = chooser.select_folder_future(Some(parent));
    let weak_parent = parent.downgrade();
    glib::MainContext::default().spawn_local(async move {
        if let Some(parent) = weak_parent.upgrade() {
            match future.await {
                Ok(directory) => scan_and_preview(&parent, state, vec![directory]),
                Err(error) if error.matches(gtk::DialogError::Dismissed) => {}
                Err(_) => preview(
                    &parent,
                    state,
                    vec![(
                        tr!("Каталог", "Folder").into(),
                        Err(tr!(
                            "Не удалось выбрать каталог.",
                            "Could not select the folder."
                        )
                        .into()),
                    )],
                ),
            }
        }
    });
}

fn scan_and_preview(
    parent: &adw::ApplicationWindow,
    state: Rc<State>,
    directories: Vec<gio::File>,
) {
    let loading = adw::Dialog::builder()
        .title(tr!("Поиск подключений Remmina", "Find Remmina Profiles"))
        .content_width(400)
        .build();
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 12);
    layout.append(&adw::HeaderBar::new());
    layout.append(
        &gtk::Spinner::builder()
            .spinning(true)
            .margin_bottom(24)
            .build(),
    );
    loading.set_child(Some(&layout));
    loading.present(Some(parent));
    let closed = Rc::new(std::cell::Cell::new(false));
    let cancelled = closed.clone();
    loading.connect_closed(move |_| cancelled.set(true));
    let weak_parent = parent.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let entries = scan(directories).await;
        if !closed.get() {
            loading.close();
            if let Some(parent) = weak_parent.upgrade() {
                preview(&parent, state, entries);
            }
        }
    });
}

async fn scan(directories: Vec<gio::File>) -> Vec<(String, Result<(Profile, String)>)> {
    let mut entries = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut count = 0;
    for directory in directories {
        if !visited.insert(directory.uri().to_string()) {
            continue;
        }
        let enumerator = match directory
            .enumerate_children_future(
                "standard::name,standard::type",
                gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                glib::Priority::DEFAULT,
            )
            .await
        {
            Ok(enumerator) => enumerator,
            Err(_) => {
                entries.push((
                    tr!("Каталог Remmina", "Remmina Folder").into(),
                    Err(tr!(
                        "Не удалось прочитать каталог.",
                        "Could not read the folder."
                    )
                    .into()),
                ));
                continue;
            }
        };
        loop {
            let files = match enumerator
                .next_files_future(64, glib::Priority::DEFAULT)
                .await
            {
                Ok(files) => files,
                Err(_) => {
                    entries.push((
                        tr!("Каталог Remmina", "Remmina Folder").into(),
                        Err(tr!(
                            "Ошибка чтения списка файлов.",
                            "Could not read the file list."
                        )
                        .into()),
                    ));
                    break;
                }
            };
            if files.is_empty() {
                break;
            }
            for info in files {
                if info.file_type() != gio::FileType::Regular
                    || info.name().extension() != Some(std::ffi::OsStr::new("remmina"))
                {
                    continue;
                }
                count += 1;
                if count > 2000 {
                    return vec![(tr!("Каталог Remmina", "Remmina Folder").into(), Err(tr!("Найдено более 2000 файлов. Выберите меньший каталог; импорт не выполнен.", "More than 2,000 files found. Choose a smaller folder before importing.").into()))];
                }
                let file = directory.child(info.name());
                entries.push((
                    info.name().to_string_lossy().into_owned(),
                    read_profile(&file).await,
                ));
            }
        }
        let _ = enumerator.close_future(glib::Priority::DEFAULT).await;
    }
    if entries.is_empty() {
        entries.push((tr!("Поиск Remmina", "Find Remmina Profiles").into(), Err(tr!("Файлы .remmina не найдены. Выберите каталог вручную. Подкаталоги и символические ссылки не просматриваются.", "No .remmina files found. Choose the folder manually. Subfolders and symbolic links are skipped.").into())));
    }
    entries
}

pub fn choose(parent: &adw::ApplicationWindow, state: Rc<State>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(tr!(
        "Профили Remmina (*.remmina)",
        "Remmina profiles (*.remmina)"
    )));
    filter.add_pattern("*.remmina");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let chooser = gtk::FileDialog::builder()
        .title(tr!("Импорт профилей Remmina", "Import Remmina Profiles"))
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
                                tr!("Выбор файлов", "Choose Files").into(),
                                Err(tr!("Не удалось выбрать файлы.", "Could not select files.")
                                    .into()),
                            )],
                        );
                    }
                }
                return;
            }
        };
        if files.n_items() > 2000 {
            if let Some(parent) = weak_parent.upgrade() {
                preview(
                    &parent,
                    state,
                    vec![(
                        tr!("Выбор файлов", "Choose Files").into(),
                        Err(tr!(
                            "Выберите не более 2000 файлов за один импорт.",
                            "Select up to 2,000 files per import."
                        )
                        .into()),
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
                    .unwrap_or_else(|| tr!("Файл Remmina", "Remmina File").into());
                entries.push((name, read_profile(&file).await));
            }
        }
        if let Some(parent) = weak_parent.upgrade() {
            preview(&parent, state, entries);
        }
    });
}

async fn read_profile(file: &gio::File) -> Result<(Profile, String)> {
    let stream = file
        .read_future(glib::Priority::DEFAULT)
        .await
        .map_err(|_| tr!("Не удалось открыть файл.", "Could not open the file."))?;
    let result = async {
        let mut data = Vec::new();
        loop {
            let bytes = stream
                .read_bytes_future(65536, glib::Priority::DEFAULT)
                .await
                .map_err(|_| tr!("Не удалось прочитать файл.", "Could not read the file."))?;
            if bytes.is_empty() {
                break;
            }
            if data.len() + bytes.len() > 1024 * 1024 {
                return Err(tr!("Файл превышает 1 МиБ.", "The file exceeds 1 MiB.").into());
            }
            data.extend_from_slice(&bytes);
        }
        let text = std::str::from_utf8(&data)
            .map_err(|_| tr!("Файл должен быть в UTF-8.", "The file must use UTF-8."))?;
        remmina::parse(text)
    }
    .await;
    let _ = stream.close_future(glib::Priority::DEFAULT).await;
    result
}

fn preview(
    parent: &adw::ApplicationWindow,
    state: Rc<State>,
    entries: Vec<(String, Result<(Profile, String)>)>,
) {
    let dialog = adw::Dialog::builder()
        .title(tr!("Импорт Remmina — просмотр", "Remmina Import Preview"))
        .content_width(620)
        .content_height(560)
        .build();
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 12);
    layout.append(&adw::HeaderBar::new());
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_start(16);
    content.set_margin_end(16);
    content.set_margin_bottom(16);
    let notice = gtk::Label::new(Some(tr!("Переносятся только название, сервер, порт, пользователь и домен. Пароли, шлюзы, туннели, команды и остальные настройки не переносятся. Одинаковые профили будут пропущены. Снимите отметки с ненужных профилей.", "Imports names, groups, servers, ports, usernames and domains. Identical profiles are skipped. Select the profiles you want to import.")));
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
            Ok((profile, folder)) => {
                row.set_title(&profile.name);
                row.set_subtitle(&tr_format!(
                    "{} · порт {} · пользователь: {} · домен: {}\nПапка: {} · {}",
                    "{} · port {} · username: {} · domain: {}\nFolder: {} · {}",
                    profile.host,
                    profile.port,
                    profile.username,
                    profile.domain,
                    if folder.is_empty() { "—" } else { &folder },
                    filename
                ));
                let check = gtk::CheckButton::builder()
                    .active(true)
                    .valign(gtk::Align::Center)
                    .build();
                row.add_prefix(&check);
                row.set_activatable_widget(Some(&check));
                candidates.push((check, (profile, folder)));
            }
            Err(error) => {
                row.set_title(&filename);
                row.set_subtitle(&error.to_string());
                row.add_css_class("error");
            }
        }
        list.append(&row);
    }
    let candidates = Rc::new(candidates);
    let selection = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    for (text, active) in [
        (tr!("Выбрать все", "Select All"), true),
        (tr!("Снять отметки", "Clear Selection"), false),
    ] {
        let button = gtk::Button::with_label(text);
        let candidates = candidates.clone();
        button.connect_clicked(move |_| {
            for (check, _) in candidates.iter() {
                check.set_active(active);
            }
        });
        selection.append(&button);
    }
    content.append(&selection);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    content.append(&scroll);
    let status = gtk::Label::new(None);
    status.set_wrap(true);
    content.append(&status);
    let import = gtk::Button::with_label(tr!("Импортировать отмеченные", "Import Selected"));
    import.add_css_class("suggested-action");
    import.set_sensitive(!candidates.is_empty());
    content.append(&import);
    import.connect_clicked(move |button| {
        let profiles: Vec<_> = candidates.iter().filter(|(check, _)| check.is_active())
            .map(|(_, profile)| profile.clone()).collect();
        if profiles.is_empty() {
            status.set_text(tr!("Отметьте хотя бы один профиль.", "Select at least one profile."));
            return;
        }
        match state.store.import_with_folders(&profiles) {
            Ok(added) => {
                profiles_ui::refresh(&state);
                status.set_text(&tr_format!("Добавлено: {added}. Дубликатов пропущено: {}.", "Added: {added}. Duplicates skipped: {}.", profiles.len() - added));
                button.set_sensitive(false);
                for (check, _) in candidates.iter() { check.set_sensitive(false); }
            }
            Err(_) => {
                status.set_text(tr!("Импорт не выполнен. База данных недоступна или произошла ошибка записи; изменения отменены.", "Import failed because the database was unavailable or could not be written. Changes were rolled back."));
            }
        }
    });
    layout.append(&content);
    dialog.set_child(Some(&layout));
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_scan_lists_valid_and_invalid_files_without_following_links() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("office.remmina"),
            "[remmina]\nprotocol=RDP\nname=Office\nserver=server.test",
        )
        .unwrap();
        std::fs::write(
            directory.path().join("ssh.remmina"),
            "[remmina]\nprotocol=SSH\nserver=server.test",
        )
        .unwrap();
        std::fs::write(directory.path().join("broken.remmina"), "broken").unwrap();
        std::fs::write(directory.path().join("unrelated.txt"), "not imported").unwrap();
        let nested = directory.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(
            nested.join("nested.remmina"),
            "[remmina]\nprotocol=RDP\nserver=other.test",
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            directory.path().join("office.remmina"),
            directory.path().join("link.remmina"),
        )
        .unwrap();
        let folder = gio::File::for_path(directory.path());
        let entries = glib::MainContext::new().block_on(scan(vec![folder.clone(), folder]));
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries.iter().filter(|(_, result)| result.is_ok()).count(),
            1
        );
    }
}
