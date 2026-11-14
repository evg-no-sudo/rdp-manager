use adw::prelude::*;

pub fn create_window(app: &adw::Application) -> (adw::ApplicationWindow, adw::TabView) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Linux RDP Manager")
        .default_width(1100)
        .default_height(720)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let toggle = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Показать профили")
        .active(true)
        .build();
    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Новая вкладка")
        .build();
    let detach = gtk::Button::with_label("В отдельное окно");
    header.pack_start(&toggle);
    header.pack_start(&add);
    header.pack_end(&detach);
    root.append(&header);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 12);
    sidebar.set_width_request(240);
    sidebar.set_margin_top(16);
    sidebar.set_margin_start(16);
    sidebar.set_margin_end(16);
    sidebar.append(&gtk::Label::new(Some("Профили подключений")));
    let note = gtk::Label::new(Some(
        "Список пока пуст.\nХранение профилей и импорт Remmina появятся на следующем этапе.",
    ));
    note.set_wrap(true);
    sidebar.append(&note);
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideRight)
        .reveal_child(true)
        .child(&sidebar)
        .build();
    toggle
        .bind_property("active", &revealer, "reveal-child")
        .sync_create()
        .build();
    body.append(&revealer);

    let tabs = adw::TabView::new();
    tabs.set_hexpand(true);
    tabs.set_vexpand(true);
    let bar = adw::TabBar::builder().view(&tabs).autohide(false).build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_hexpand(true);
    content.append(&bar);
    content.append(&tabs);
    body.append(&content);
    root.append(&body);
    window.set_content(Some(&root));
    append_tab(&tabs);

    let weak_tabs = tabs.downgrade();
    add.connect_clicked(move |_| {
        if let Some(tabs) = weak_tabs.upgrade() {
            append_tab(&tabs);
        }
    });
    let weak_app = app.downgrade();
    let weak_tabs = tabs.downgrade();
    detach.connect_clicked(move |_| {
        if let (Some(app), Some(tabs)) = (weak_app.upgrade(), weak_tabs.upgrade()) {
            if let Some(page) = tabs.selected_page() {
                let (window, target) = create_window(&app);
                // Remove the new window's initial placeholder before transferring the page.
                if let Some(initial) = target.selected_page() {
                    target.close_page(&initial);
                }
                tabs.transfer_page(&page, &target, 0);
                window.present();
            }
        }
    });
    (window, tabs)
}

fn append_tab(tabs: &adw::TabView) {
    let page = adw::StatusPage::builder()
        .icon_name("network-server-symbolic")
        .title("Новая RDP-сессия")
        .description(
            "Интерфейс готов. Подключение через FreeRDP будет добавлено на следующем этапе.",
        )
        .build();
    let tab = tabs.append(&page);
    tab.set_title("Новая сессия");
    tabs.set_selected_page(&tab);
}
