use crate::ui::State;
use adw::prelude::*;
use std::rc::Rc;

pub fn show(parent: &adw::ApplicationWindow, _: Rc<State>) {
    let dialog = adw::Dialog::builder()
        .title(tr!("Что нового", "What's New"))
        .content_width(580)
        .content_height(500)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&adw::HeaderBar::new());
    let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(20);
    content.set_margin_end(20);

    let version = format!(
        "{}-{}",
        env!("CARGO_PKG_VERSION"),
        include_str!("../data/deb-revision").trim()
    );
    content.append(
        &gtk::Label::builder()
            .label(tr_format!(
                "Текущая сборка: {version}",
                "Current build: {version}"
            ))
            .xalign(0.0)
            .build(),
    );
    for (number, changes) in [
        ("0.1.0-26", tr!("Сохраняются размер главного окна и раскрытые папки. Полноэкранное меню закрывается по клику снаружи.", "The main window size and expanded folders are remembered. Click outside the fullscreen menu to dismiss it.")),
        ("0.1.0-25", tr!("Добавлена история изменений в меню приложения.", "Added release notes to the application menu.")),
        ("0.1.0-22 — 0.1.0-24", tr!("Правый клик по полноэкранной панели открывает список подключений. Порядок совпадает с боковой панелью. Группы свёрнуты по умолчанию.", "Right-click the fullscreen toolbar to open saved connections. The order matches the sidebar. Groups start collapsed.")),
        ("0.1.0-20 — 0.1.0-21", tr!("Захват клавиш восстанавливается при возврате в сеанс. Добавлены коды цифрового блока и синхронизация Num Lock.", "Keyboard capture is restored when returning to a session. Added numeric keypad support and Num Lock synchronization.")),
        ("0.1.0-18 — 0.1.0-19", tr!("Убрана пустая вкладка «Новая сессия». Добавлены английский интерфейс, выбор языка и двуязычная документация.", "Removed the placeholder session tab. Added an English interface, a language selector and bilingual documentation.")),
        ("0.1.0-15 — 0.1.0-17", tr!("Папки подключений, импорт групп Remmina и сортировка. Закрытие вкладки в полном экране переключает на соседнюю. Включено опережающее чтение файлов из буфера.", "Added profile folders, Remmina group import and sorting. Closing a fullscreen tab selects its neighbor. Enabled read-ahead for clipboard files.")),
        ("0.1.0-8 — 0.1.0-14", tr!("Передача файлов и папок между сеансами через буфер обмена, потоковая отправка и счётчик переданных данных. Вкладки и кнопки размещены в одной строке полноэкранного режима.", "Added streamed clipboard transfers of files and folders between sessions, with a byte counter. Fullscreen tabs and controls share one row.")),
        ("0.1.0 — 0.1.0-7", tr!("Первый пакет DEB для Mint 22.3, значок приложения, меню обмена профилями, возврат вкладки из отдельного окна и выбор темы.", "First DEB for Mint 22.3, desktop icon, profile exchange menu, tab return from separate windows, and theme selection.")),
    ] {
        let release = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let heading = gtk::Label::builder().label(number).xalign(0.0).build();
        heading.add_css_class("heading");
        release.append(&heading);
        release.append(&gtk::Label::builder().label(changes).xalign(0.0).wrap(true).selectable(true).build());
        content.append(&release);
    }

    root.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build(),
    );
    dialog.set_child(Some(&root));
    dialog.present(Some(parent));
}
