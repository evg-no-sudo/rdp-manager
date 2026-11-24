use crate::store::{Profile, Store};
use adw::prelude::*;
use std::{cell::RefCell, collections::HashSet, rc::Rc};
type SessionStop = (gtk::glib::WeakRef<gtk::Widget>, Rc<dyn Fn()>);

pub struct State {
    pub store: Store,
    pub lists: RefCell<Vec<gtk::glib::WeakRef<gtk::ListBox>>>,
    operations: RefCell<HashSet<String>>,
    session_stops: RefCell<Vec<SessionStop>>,
    views: RefCell<
        Vec<(
            gtk::glib::WeakRef<adw::ApplicationWindow>,
            gtk::glib::WeakRef<adw::TabView>,
        )>,
    >,
}

impl State {
    pub fn new(store: Store) -> Rc<Self> {
        Rc::new(Self {
            store,
            lists: RefCell::new(Vec::new()),
            operations: RefCell::new(HashSet::new()),
            session_stops: RefCell::new(Vec::new()),
            views: RefCell::new(Vec::new()),
        })
    }

    pub fn tabs_for(&self, target: &adw::ApplicationWindow) -> Option<adw::TabView> {
        self.views.borrow().iter().find_map(|(window, tabs)| {
            if window.upgrade().as_ref() == Some(target) {
                tabs.upgrade()
            } else {
                None
            }
        })
    }

    pub fn register_session(&self, widget: &impl IsA<gtk::Widget>, stop: Rc<dyn Fn()>) {
        let mut sessions = self.session_stops.borrow_mut();
        sessions.retain(|(widget, _)| widget.upgrade().is_some());
        sessions.push((widget.as_ref().downgrade(), stop));
    }
    pub fn close_session(&self, widget: &gtk::Widget) {
        let stop = self
            .session_stops
            .borrow()
            .iter()
            .find_map(|(candidate, stop)| {
                if candidate.upgrade().as_ref() == Some(widget) {
                    Some(stop.clone())
                } else {
                    None
                }
            });
        if let Some(stop) = stop {
            stop();
        }
    }

    pub fn operation(self: &Rc<Self>, uid: &str) -> Option<Operation> {
        if !self.operations.borrow_mut().insert(uid.into()) {
            return None;
        }
        Some(Operation {
            state: self.clone(),
            uid: uid.into(),
        })
    }
}

pub struct Operation {
    state: Rc<State>,
    uid: String,
}

impl Drop for Operation {
    fn drop(&mut self) {
        self.state.operations.borrow_mut().remove(&self.uid);
    }
}

pub fn create_window(
    app: &adw::Application,
    state: Rc<State>,
) -> (adw::ApplicationWindow, adw::TabView) {
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

    let body = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .position(260)
        .resize_start_child(false)
        .shrink_start_child(false)
        .resize_end_child(true)
        .shrink_end_child(true)
        .wide_handle(true)
        .vexpand(true)
        .build();
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 12);
    sidebar.set_width_request(180);
    sidebar.set_hexpand(false);
    sidebar.set_margin_top(16);
    sidebar.set_margin_start(16);
    sidebar.set_margin_end(16);
    let sidebar_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::builder()
        .label("Подключения")
        .xalign(0.0)
        .hexpand(true)
        .build();
    sidebar_header.append(&title);
    let menu = gtk::gio::Menu::new();
    let import_menu = gtk::gio::Menu::new();
    import_menu.append(
        Some("Найти подключения Remmina"),
        Some("win.discover-remmina"),
    );
    import_menu.append(
        Some("Выбрать каталог…"),
        Some("win.import-remmina-directory"),
    );
    import_menu.append(Some("Выбрать файлы…"), Some("win.import-remmina"));
    menu.append_submenu(Some("Импорт Remmina"), &import_menu);
    let sync_menu = gtk::gio::Menu::new();
    sync_menu.append(Some("Через папку…"), Some("win.transfer-profiles"));
    sync_menu.append(Some("Через WebDAV / Nextcloud…"), Some("win.webdav"));
    sync_menu.append(Some("Через SFTP / SSH…"), Some("win.sftp"));
    menu.append_submenu(Some("Синхронизация подключений"), &sync_menu);
    menu.append(Some("История подключений…"), Some("win.history"));
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Импорт, синхронизация и история подключений")
        .menu_model(&menu)
        .build();
    more.add_css_class("flat");
    sidebar_header.append(&more);
    sidebar.append(&sidebar_header);
    let new_profile = gtk::Button::with_label("Новое подключение");
    sidebar.append(&new_profile);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Поиск подключений")
        .build();
    sidebar.append(&search);
    let transfer = gtk::gio::SimpleAction::new("transfer-profiles", None);
    let weak_window = window.downgrade();
    let transfer_state = state.clone();
    transfer.connect_activate(move |_, _| {
        if let Some(window) = weak_window.upgrade() {
            crate::transfer_ui::show(&window, transfer_state.clone());
        }
    });
    window.add_action(&transfer);
    let import = gtk::gio::SimpleAction::new("import-remmina", None);
    let weak_window = window.downgrade();
    let import_state = state.clone();
    import.connect_activate(move |_, _| {
        if let Some(window) = weak_window.upgrade() {
            crate::import_ui::choose(&window, import_state.clone());
        }
    });
    window.add_action(&import);
    for (name, callback) in [
        (
            "discover-remmina",
            crate::import_ui::discover as fn(&adw::ApplicationWindow, Rc<State>),
        ),
        (
            "import-remmina-directory",
            crate::import_ui::choose_directory,
        ),
        ("webdav", crate::webdav_ui::show),
        ("sftp", crate::sftp_ui::show),
        ("history", crate::rdp_ui::history),
    ] {
        let action = gtk::gio::SimpleAction::new(name, None);
        let weak_window = window.downgrade();
        let state = state.clone();
        action.connect_activate(move |_, _| {
            if let Some(window) = weak_window.upgrade() {
                callback(&window, state.clone());
            }
        });
        window.add_action(&action);
    }
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_valign(gtk::Align::Start);
    list.set_vexpand(false);
    list.add_css_class("boxed-list");
    let search_for_filter = search.downgrade();
    list.set_filter_func(move |row| {
        let Some(search) = search_for_filter.upgrade() else {
            return true;
        };
        let query = search.text().trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        row.downcast_ref::<adw::ActionRow>().is_some_and(|profile| {
            profile.title().to_lowercase().contains(&query)
                || profile
                    .subtitle()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
        })
    });
    let weak_list = list.downgrade();
    search.connect_search_changed(move |_| {
        if let Some(list) = weak_list.upgrade() {
            list.invalidate_filter();
        }
    });
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    sidebar.append(&scroll);
    state.lists.borrow_mut().push(list.downgrade());
    crate::profiles_ui::refresh(&state);
    let weak_window = window.downgrade();
    let profile_state = state.clone();
    new_profile.connect_clicked(move |_| {
        if let Some(window) = weak_window.upgrade() {
            crate::profiles_ui::edit(&window, profile_state.clone(), Profile::default());
        }
    });
    body.set_start_child(Some(&sidebar));
    let weak_body = body.downgrade();
    let weak_sidebar = sidebar.downgrade();
    let sidebar_width = std::cell::Cell::new(260);
    toggle.connect_toggled(move |toggle| {
        if let (Some(body), Some(sidebar)) = (weak_body.upgrade(), weak_sidebar.upgrade()) {
            if toggle.is_active() {
                sidebar.set_visible(true);
                body.set_position(sidebar_width.get());
            } else {
                sidebar_width.set(body.position());
                sidebar.set_visible(false);
            }
        }
    });

    let tabs = adw::TabView::new();
    let close_state = state.clone();
    tabs.connect_close_page(move |_, page| {
        close_state.close_session(&page.child());
        gtk::glib::Propagation::Proceed
    });
    let weak_tabs = tabs.downgrade();
    let close_state = state.clone();
    window.connect_close_request(move |_| {
        if let Some(tabs) = weak_tabs.upgrade() {
            for index in 0..tabs.n_pages() {
                close_state.close_session(&tabs.nth_page(index).child());
            }
        }
        gtk::glib::Propagation::Proceed
    });
    state
        .views
        .borrow_mut()
        .retain(|(window, tabs)| window.upgrade().is_some() && tabs.upgrade().is_some());
    state
        .views
        .borrow_mut()
        .push((window.downgrade(), tabs.downgrade()));
    tabs.set_hexpand(true);
    tabs.set_vexpand(true);
    let bar = adw::TabBar::builder().view(&tabs).autohide(false).build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_hexpand(true);
    content.append(&bar);
    content.append(&tabs);
    body.set_end_child(Some(&content));
    root.append(&body);
    let fullscreen_header = header.downgrade();
    let fullscreen_sidebar = sidebar.downgrade();
    let sidebar_toggle = toggle.downgrade();
    window.connect_fullscreened_notify(move |window| {
        let fullscreen = window.is_fullscreen();
        if let Some(header) = fullscreen_header.upgrade() {
            header.set_visible(!fullscreen);
        }
        if let (Some(sidebar), Some(toggle)) =
            (fullscreen_sidebar.upgrade(), sidebar_toggle.upgrade())
        {
            sidebar.set_visible(!fullscreen && toggle.is_active());
        }
    });
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
                let (window, target) = create_window(&app, state.clone());
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
            "Нажмите кнопку подключения справа от нужного профиля в боковой панели. Параметры профиля открываются нажатием на его название.",
        )
        .build();
    let tab = tabs.append(&page);
    tab.set_title("Новая сессия");
    tabs.set_selected_page(&tab);
}
