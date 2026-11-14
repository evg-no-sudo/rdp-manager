use crate::store::{Profile, Store};
use adw::prelude::*;
use std::{cell::RefCell, collections::HashSet, rc::Rc};

pub struct State {
    pub store: Store,
    pub lists: RefCell<Vec<gtk::glib::WeakRef<gtk::ListBox>>>,
    operations: RefCell<HashSet<String>>,
}

impl State {
    pub fn new(store: Store) -> Rc<Self> {
        Rc::new(Self {
            store,
            lists: RefCell::new(Vec::new()),
            operations: RefCell::new(HashSet::new()),
        })
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

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 12);
    sidebar.set_width_request(240);
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
    menu.append(Some("Импорт Remmina…"), Some("win.import-remmina"));
    menu.append(Some("Обмен через папку…"), Some("win.transfer-profiles"));
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Импорт и обмен подключениями")
        .menu_model(&menu)
        .build();
    more.add_css_class("flat");
    sidebar_header.append(&more);
    sidebar.append(&sidebar_header);
    let new_profile = gtk::Button::with_label("Новое подключение");
    sidebar.append(&new_profile);
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
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");
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
            "Интерфейс готов. Подключение через FreeRDP будет добавлено на следующем этапе.",
        )
        .build();
    let tab = tabs.append(&page);
    tab.set_title("Новая сессия");
    tabs.set_selected_page(&tab);
}
