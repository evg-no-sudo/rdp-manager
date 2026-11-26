use crate::store::{Profile, Store};
use adw::prelude::*;
use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    io::Write,
    rc::Rc,
};
type SessionStop = (gtk::glib::WeakRef<gtk::Widget>, Rc<dyn Fn()>);

pub(crate) struct ProfileList {
    pub(crate) list: gtk::glib::WeakRef<gtk::ListBox>,
    pub(crate) sort: gtk::glib::WeakRef<gtk::gio::SimpleAction>,
}

impl ProfileList {
    pub(crate) fn frequently(&self) -> bool {
        self.sort
            .upgrade()
            .and_then(|sort| sort.state())
            .is_some_and(|value| value.str() == Some("frequent"))
    }
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
struct ViewPreferences {
    window_size: Option<(i32, i32)>,
    expanded_folders: HashSet<String>,
    fullscreen_folder: Option<String>,
}

fn view_preferences_path() -> std::path::PathBuf {
    gtk::glib::user_config_dir().join("rdp-manager/view.json")
}

pub struct State {
    pub store: Store,
    pub(crate) lists: RefCell<Vec<ProfileList>>,
    pub(crate) expanded_folders: RefCell<HashSet<String>>,
    pub(crate) fullscreen_folder: RefCell<Option<String>>,
    window_size: Cell<Option<(i32, i32)>>,
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
        let preferences: ViewPreferences = std::fs::read(view_preferences_path())
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        Rc::new(Self {
            store,
            lists: RefCell::new(Vec::new()),
            expanded_folders: RefCell::new(preferences.expanded_folders),
            fullscreen_folder: RefCell::new(preferences.fullscreen_folder),
            window_size: Cell::new(
                preferences
                    .window_size
                    .filter(|&(w, h)| (320..=16384).contains(&w) && (240..=16384).contains(&h)),
            ),
            operations: RefCell::new(HashSet::new()),
            session_stops: RefCell::new(Vec::new()),
            views: RefCell::new(Vec::new()),
        })
    }

    pub(crate) fn save_view_preferences(&self) {
        let save = || -> std::io::Result<()> {
            let path = view_preferences_path();
            let dir = gtk::glib::user_config_dir().join("rdp-manager");
            std::fs::create_dir_all(&dir)?;
            let mut file = tempfile::NamedTempFile::new_in(dir)?;
            serde_json::to_writer(
                &mut file,
                &ViewPreferences {
                    window_size: self.window_size.get(),
                    expanded_folders: self.expanded_folders.borrow().clone(),
                    fullscreen_folder: self.fullscreen_folder.borrow().clone(),
                },
            )?;
            file.flush()?;
            file.persist(path)?;
            Ok(())
        };
        if let Err(error) = save() {
            eprintln!("Could not save view preferences: {error}");
        }
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

fn place_session_toolbar(
    tabs: &adw::TabView,
    bar: &adw::TabBar,
    owner: &RefCell<Option<gtk::glib::WeakRef<gtk::Box>>>,
    fullscreen: bool,
) {
    use gtk::pango::EllipsizeMode;

    let page = tabs
        .selected_page()
        .and_then(|page| page.child().downcast::<gtk::Box>().ok());
    let previous = owner.borrow().as_ref().and_then(|owner| owner.upgrade());
    if fullscreen && previous.is_some() && previous == page {
        return;
    }
    if let Some(toolbar) = bar.end_action_widget() {
        bar.set_end_action_widget(None::<&gtk::Widget>);
        if let Some(status) = toolbar.first_child().and_downcast::<gtk::Label>() {
            status.set_wrap(true);
            status.set_ellipsize(EllipsizeMode::None);
            status.set_max_width_chars(-1);
        }
        if let Some(previous) = previous {
            previous.prepend(&toolbar);
        }
    }
    owner.replace(None);
    if fullscreen {
        if let Some(page) = page {
            if let Some(toolbar) = page
                .first_child()
                .filter(|child| child.widget_name() == crate::rdp_ui::SESSION_TOOLBAR)
            {
                if let Some(status) = toolbar.first_child().and_downcast::<gtk::Label>() {
                    status.set_wrap(false);
                    status.set_max_width_chars(18);
                    status.set_ellipsize(EllipsizeMode::End);
                }
                page.remove(&toolbar);
                bar.set_end_action_widget(Some(&toolbar));
                owner.replace(Some(page.downgrade()));
            }
        }
    }
}

fn install_fullscreen_connections(
    bar: &adw::TabBar,
    tabs: &adw::TabView,
    window: &adw::ApplicationWindow,
    state: &Rc<State>,
) {
    let (weak_tabs, weak_window) = (tabs.downgrade(), window.downgrade());
    let clicks = gtk::EventControllerLegacy::new();
    clicks.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_bar = bar.downgrade();
    let active_popup = gtk::glib::WeakRef::<gtk::Popover>::new();
    let trace = std::env::var_os("RDP_MANAGER_TRACE_MENU").is_some();
    let state = state.clone();
    clicks.connect_event(move |_, event| {
        use gtk::glib::Propagation;

        let Some(button) = event.downcast_ref::<gtk::gdk::ButtonEvent>() else {
            return Propagation::Proceed;
        };
        if event.event_type() != gtk::gdk::EventType::ButtonPress || button.button() != 3 {
            return Propagation::Proceed;
        }
        let (Some(bar), Some(tabs), Some(window)) = (
            weak_bar.upgrade(),
            weak_tabs.upgrade(),
            weak_window.upgrade(),
        ) else {
            return Propagation::Proceed;
        };
        let same_surface = event.surface() == window.surface();
        let position = event.position();
        if trace {
            eprintln!(
                "[rdp-menu] press surface={same_surface} fullscreen={} position={position:?}",
                window.is_fullscreen()
            );
        }
        if !window.is_fullscreen() || !same_surface || !bar.is_mapped() {
            return Propagation::Proceed;
        }
        let Some((x, y)) = position else {
            return Propagation::Proceed;
        };
        let (surface_x, surface_y) = window.surface_transform();
        let point = gtk::graphene::Point::new((x + surface_x) as f32, (y + surface_y) as f32);
        let Some(point) = window.compute_point(&bar, &point) else {
            return Propagation::Proceed;
        };
        let (x, y) = (f64::from(point.x()), f64::from(point.y()));
        if trace {
            eprintln!(
                "[rdp-menu] panel x={x:.1} y={y:.1} width={} height={}",
                bar.width(),
                bar.height()
            );
        }
        if x < 0.0 || y < 0.0 || x >= f64::from(bar.width()) || y >= f64::from(bar.height()) {
            return Propagation::Proceed;
        }
        let mut hit = bar.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(widget) = hit {
            if widget == bar {
                break;
            }
            if widget.is::<gtk::Button>()
                || widget.is::<gtk::MenuButton>()
                || widget.css_name() == "tab"
            {
                if trace {
                    eprintln!("[rdp-menu] control={}", widget.css_name());
                }
                return Propagation::Proceed;
            }
            hit = widget.parent();
        }

        if let Some(previous) = active_popup.upgrade() {
            previous.popdown();
        }
        let popup = gtk::Popover::builder()
            .autohide(false)
            .position(gtk::PositionType::Bottom)
            .build();
        active_popup.set(Some(&popup));
        let items = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let max_height = (window.height() - bar.height() - 48).clamp(1, 480);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .overlay_scrolling(false)
            .child(&items)
            .build();
        popup.set_child(Some(&scroll));
        popup.set_parent(&bar);
        let resize = Rc::new({
            let (items, scroll) = (items.downgrade(), scroll.downgrade());
            move || {
                if let (Some(items), Some(scroll)) = (items.upgrade(), scroll.upgrade()) {
                    let (_, height, _, _) = items.measure(gtk::Orientation::Vertical, -1);
                    scroll.set_height_request(height.clamp(1, max_height));
                }
            }
        });
        let mut connection_rows = Vec::new();
        let folders = state.store.profile_folders().unwrap_or_default();
        match state.store.profiles() {
            Ok(profiles) if !profiles.is_empty() => {
                let frequently = window
                    .lookup_action("sort-profiles")
                    .and_then(|action| action.state())
                    .is_some_and(|value| value.str() == Some("frequent"));
                let counts = state.store.connection_counts().unwrap_or_default();
                let mut profiles: Vec<_> = profiles.iter().collect();
                crate::profiles_ui::sort_profiles(&mut profiles, &folders, &counts, frequently);
                let mut previous = None;
                let mut group = items.clone();
                for profile in profiles {
                    let folder = folders.get(&profile.uid).map(String::as_str).unwrap_or("");
                    if previous != Some(folder) {
                        group = gtk::Box::new(gtk::Orientation::Vertical, 0);
                        let title = if folder.is_empty() {
                            tr!("Без папки", "Ungrouped")
                        } else {
                            folder
                        };
                        let expander = gtk::Expander::builder()
                            .label(title)
                            .expanded(state.fullscreen_folder.borrow().as_deref() == Some(folder))
                            .child(&group)
                            .build();
                        let state = state.clone();
                        let folder_name = folder.to_owned();
                        let resize = resize.clone();
                        expander.connect_expanded_notify(move |expander| {
                            if expander.is_expanded() {
                                state.fullscreen_folder.replace(Some(folder_name.clone()));
                                let mut sibling =
                                    expander.parent().and_then(|parent| parent.first_child());
                                while let Some(widget) = sibling {
                                    sibling = widget.next_sibling();
                                    if let Ok(other) = widget.downcast::<gtk::Expander>() {
                                        if other != *expander {
                                            other.set_expanded(false);
                                        }
                                    }
                                }
                            } else if state.fullscreen_folder.borrow().as_deref()
                                == Some(&folder_name)
                            {
                                state.fullscreen_folder.replace(None);
                            } else {
                                return;
                            }
                            let state = state.clone();
                            let resize = resize.clone();
                            gtk::glib::idle_add_local_once(move || {
                                resize();
                                state.save_view_preferences();
                            });
                        });
                        items.append(&expander);
                        previous = Some(folder);
                    }
                    let label = gtk::Label::builder()
                        .label(&profile.name)
                        .xalign(0.0)
                        .build();
                    let button = gtk::Button::builder()
                        .child(&label)
                        .tooltip_text(&profile.name)
                        .build();
                    button.add_css_class("fullscreen-server");
                    button.add_css_class("flat");
                    let (popup, tabs, window) =
                        (popup.downgrade(), tabs.downgrade(), window.downgrade());
                    let state = state.clone();
                    let profile = profile.clone();
                    button.connect_clicked(move |_| {
                        if let Some(popup) = popup.upgrade() {
                            popup.popdown();
                        }
                        if let (Some(tabs), Some(window)) = (tabs.upgrade(), window.upgrade()) {
                            crate::rdp_ui::connect(&window, &tabs, state.clone(), profile.clone());
                        }
                    });
                    group.append(&button);
                    connection_rows.push((button, label));
                }
            }
            Ok(_) => items.append(&gtk::Label::new(Some(tr!(
                "Нет сохранённых подключений",
                "No saved connections"
            )))),
            Err(_) => items.append(&gtk::Label::new(Some(tr!(
                "Не удалось загрузить подключения",
                "Could not load connections"
            )))),
        }
        let (_, mut content_width, _, _) = items.measure(gtk::Orientation::Horizontal, -1);
        for (button, label) in connection_rows {
            let (_, width, _, _) = button.measure(gtk::Orientation::Horizontal, -1);
            content_width = content_width.max(width);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        }
        let max_width = (window.width() - 48).max(1);
        scroll.set_width_request((content_width + 24).min(max_width));
        resize();
        popup.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            x as i32,
            bar.height() - 1,
            1,
            1,
        )));
        let outside = gtk::GestureClick::new();
        outside.set_button(0);
        outside.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak_popup = popup.downgrade();
        outside.connect_pressed(move |gesture, _, _, _| {
            let Some(popup) = weak_popup.upgrade() else {
                return;
            };
            let clicked_surface = gesture.current_event().and_then(|event| event.surface());
            if clicked_surface.is_some() && clicked_surface != popup.surface() {
                popup.popdown();
            }
        });
        window.add_controller(outside.clone());

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak_popup = popup.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                if let Some(popup) = weak_popup.upgrade().filter(|popup| popup.is_visible()) {
                    popup.popdown();
                    return gtk::glib::Propagation::Stop;
                }
            }
            gtk::glib::Propagation::Proceed
        });
        window.add_controller(keys.clone());

        let weak_window = window.downgrade();
        let weak_outside = outside.downgrade();
        let weak_keys = keys.downgrade();
        popup.connect_closed(move |popup| {
            if let Some(window) = weak_window.upgrade() {
                if let Some(controller) = weak_outside.upgrade() {
                    window.remove_controller(&controller);
                }
                if let Some(controller) = weak_keys.upgrade() {
                    window.remove_controller(&controller);
                }
            }
            if popup.parent().is_some() {
                popup.unparent();
            }
        });
        if trace {
            eprintln!("[rdp-menu] open");
        }
        popup.popup();
        Propagation::Stop
    });
    window.add_controller(clicks);
}

pub fn create_window(
    app: &adw::Application,
    state: Rc<State>,
) -> (adw::ApplicationWindow, adw::TabView) {
    create_session_window(app, state, None)
}

fn remember_window_size(window: &adw::ApplicationWindow, state: &State) {
    if window.is_mapped() && !window.is_fullscreen() && !window.is_maximized() {
        let (width, height) = (window.width(), window.height());
        if width >= 320 && height >= 240 {
            state.window_size.set(Some((width, height)));
        }
    }
}

fn create_session_window(
    app: &adw::Application,
    state: Rc<State>,
    origin: Option<gtk::glib::WeakRef<adw::TabView>>,
) -> (adw::ApplicationWindow, adw::TabView) {
    let main_window = origin.is_none();
    let (width, height) = if main_window {
        state.window_size.get().unwrap_or((1100, 720))
    } else {
        (1100, 720)
    };
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Linux RDP Manager")
        .default_width(width)
        .default_height(height)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let toggle = gtk::ToggleButton::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text(tr!("Показать профили", "Show profiles"))
        .active(true)
        .build();
    let detach = gtk::Button::with_label(tr!("В отдельное окно", "Move to New Window"));
    header.pack_start(&toggle);
    header.pack_end(&detach);
    let return_tab = origin.as_ref().map(|_| {
        let button = gtk::Button::with_label(tr!("Вернуть вкладку", "Return Tab"));
        header.pack_end(&button);
        button
    });
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
        .label(tr!("Подключения", "Connections"))
        .xalign(0.0)
        .hexpand(true)
        .build();
    sidebar_header.append(&title);
    let menu = gtk::gio::Menu::new();
    let import_menu = gtk::gio::Menu::new();
    import_menu.append(
        Some(tr!("Найти подключения Remmina", "Find Remmina Profiles")),
        Some("win.discover-remmina"),
    );
    import_menu.append(
        Some(tr!("Выбрать каталог…", "Choose Folder…")),
        Some("win.import-remmina-directory"),
    );
    import_menu.append(
        Some(tr!("Выбрать файлы…", "Choose Files…")),
        Some("win.import-remmina"),
    );
    menu.append_submenu(
        Some(tr!("Импорт Remmina", "Import from Remmina")),
        &import_menu,
    );
    let sync_menu = gtk::gio::Menu::new();
    sync_menu.append(
        Some(tr!("Через папку…", "Folder…")),
        Some("win.transfer-profiles"),
    );
    sync_menu.append(
        Some(tr!("Через WebDAV / Nextcloud…", "WebDAV / Nextcloud…")),
        Some("win.webdav"),
    );
    sync_menu.append(
        Some(tr!("Через SFTP / SSH…", "SFTP / SSH…")),
        Some("win.sftp"),
    );
    menu.append_submenu(
        Some(tr!("Синхронизация подключений", "Profile Sync")),
        &sync_menu,
    );
    menu.append(
        Some(tr!("История подключений…", "Connection History…")),
        Some("win.history"),
    );
    let sort_menu = gtk::gio::Menu::new();
    sort_menu.append(
        Some(tr!("Часто используемые", "Most Used")),
        Some("win.sort-profiles::frequent"),
    );
    sort_menu.append(
        Some(tr!("По алфавиту", "Alphabetical")),
        Some("win.sort-profiles::alphabetical"),
    );
    menu.append_submenu(
        Some(tr!("Сортировка подключений", "Sort Connections")),
        &sort_menu,
    );
    let theme_menu = gtk::gio::Menu::new();
    theme_menu.append(
        Some(tr!("Системная", "System Default")),
        Some("app.theme::system"),
    );
    theme_menu.append(Some(tr!("Светлая", "Light")), Some("app.theme::light"));
    theme_menu.append(Some(tr!("Тёмная", "Dark")), Some("app.theme::dark"));
    menu.append_submenu(Some(tr!("Тема оформления", "Appearance")), &theme_menu);
    let language = gtk::gio::Menu::new();
    language.append(
        Some(tr!("Системный", "System default")),
        Some("app.language::system"),
    );
    language.append(Some("English"), Some("app.language::en"));
    language.append(Some("Русский"), Some("app.language::ru"));
    menu.append_submenu(
        Some(tr!("Язык интерфейса", "Interface language")),
        &language,
    );
    menu.append(
        Some(tr!("Что нового…", "What's New…")),
        Some("win.changelog"),
    );
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(tr!("Дополнительные действия", "More Actions"))
        .menu_model(&menu)
        .build();
    more.add_css_class("flat");
    sidebar_header.append(&more);
    sidebar.append(&sidebar_header);
    let new_profile = gtk::Button::with_label(tr!("Новое подключение", "New Connection"));
    sidebar.append(&new_profile);
    let search = gtk::SearchEntry::builder()
        .placeholder_text(tr!("Поиск подключений", "Search Connections"))
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
        ("changelog", crate::changelog::show),
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
    let sort = gtk::gio::SimpleAction::new_stateful(
        "sort-profiles",
        Some(gtk::glib::VariantTy::STRING),
        &"frequent".to_variant(),
    );
    window.add_action(&sort);
    let search_for_filter = search.downgrade();
    let filter_state = state.clone();
    list.set_filter_func(move |row| {
        let query = search_for_filter
            .upgrade()
            .map(|search| search.text().trim().to_lowercase())
            .unwrap_or_default();
        if query.is_empty() {
            return !row.is::<adw::ActionRow>()
                || filter_state
                    .expanded_folders
                    .borrow()
                    .contains(row.widget_name().as_str());
        }
        let matches = |profile: &adw::ActionRow| {
            profile.widget_name().to_lowercase().contains(&query)
                || profile.title().to_lowercase().contains(&query)
                || profile
                    .subtitle()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(&query)
        };
        if let Some(profile) = row.downcast_ref::<adw::ActionRow>() {
            return matches(profile);
        }
        let mut next = row.next_sibling();
        while let Some(widget) = next {
            let Some(profile) = widget.downcast_ref::<adw::ActionRow>() else {
                break;
            };
            if matches(profile) {
                return true;
            }
            next = widget.next_sibling();
        }
        false
    });
    let sort_state = state.clone();
    sort.connect_change_state(move |action, value| {
        if let Some(value) =
            value.filter(|value| matches!(value.str(), Some("frequent" | "alphabetical")))
        {
            action.set_state(value);
            crate::profiles_ui::refresh(&sort_state);
        }
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
    state.lists.borrow_mut().push(ProfileList {
        list: list.downgrade(),
        sort: sort.downgrade(),
    });
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
    let close_window = window.downgrade();
    tabs.connect_close_page(move |tabs, page| {
        if tabs.selected_page().as_ref() == Some(page) {
            if let Some(window) = close_window
                .upgrade()
                .filter(|window| window.is_fullscreen())
            {
                if tabs.n_pages() > 1 {
                    let index = tabs.page_position(page);
                    let neighbor = if index + 1 < tabs.n_pages() {
                        index + 1
                    } else {
                        index - 1
                    };
                    tabs.set_selected_page(&tabs.nth_page(neighbor));
                } else {
                    window.unfullscreen();
                }
            }
        }
        close_state.close_session(&page.child());
        gtk::glib::Propagation::Proceed
    });
    let weak_tabs = tabs.downgrade();
    let close_state = state.clone();
    if main_window {
        let size_state = state.clone();
        window.add_tick_callback(move |window, _| {
            remember_window_size(window, &size_state);
            gtk::glib::ControlFlow::Continue
        });
    }
    window.connect_close_request(move |window| {
        if main_window {
            remember_window_size(window, &close_state);
            close_state.save_view_preferences();
        }
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
    install_fullscreen_connections(&bar, &tabs, &window, &state);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_hexpand(true);
    content.append(&bar);
    let empty = adw::StatusPage::builder()
        .icon_name("network-server-symbolic")
        .title(tr!("Нет открытых подключений", "No Open Connections"))
        .description(tr!(
            "Выберите подключение в боковой панели.",
            "Choose a connection in the sidebar."
        ))
        .vexpand(true)
        .build();
    content.append(&tabs);
    content.append(&empty);
    let update_empty = {
        let (bar, empty, detach) = (bar.downgrade(), empty.downgrade(), detach.downgrade());
        move |tabs: &adw::TabView| {
            let has_pages = tabs.n_pages() > 0;
            tabs.set_visible(has_pages || tabs.is_transferring_page());
            if let Some(bar) = bar.upgrade() {
                bar.set_visible(has_pages || tabs.is_transferring_page());
            }
            if let Some(empty) = empty.upgrade() {
                empty.set_visible(!has_pages);
            }
            if let Some(detach) = detach.upgrade() {
                detach.set_sensitive(has_pages);
            }
        }
    };
    update_empty(&tabs);
    tabs.connect_n_pages_notify(update_empty.clone());
    tabs.connect_is_transferring_page_notify(update_empty);
    body.set_end_child(Some(&content));
    root.append(&body);
    let toolbar_owner = Rc::new(RefCell::new(None));
    let sync_toolbar = {
        let bar = bar.downgrade();
        let owner = toolbar_owner.clone();
        move |tabs: &adw::TabView| {
            if let Some(bar) = bar.upgrade() {
                let fullscreen = tabs
                    .root()
                    .and_downcast::<gtk::Window>()
                    .is_some_and(|window| window.is_fullscreen());
                bar.set_expand_tabs(!fullscreen);
                place_session_toolbar(tabs, &bar, &owner, fullscreen);
            }
        }
    };
    tabs.connect_selected_page_notify(sync_toolbar.clone());
    tabs.connect_selected_page_notify(|tabs| {
        let tabs = tabs.downgrade();
        gtk::glib::idle_add_local_once(move || {
            if let Some(picture) = tabs
                .upgrade()
                .and_then(|tabs| tabs.selected_page())
                .and_then(|page| page.child().last_child())
                .and_downcast::<gtk::Picture>()
                .filter(|picture| picture.is_mapped() && picture.is_sensitive())
            {
                picture.grab_focus();
            }
        });
    });
    let sync_attached = sync_toolbar.clone();
    tabs.connect_page_attached(move |tabs, _, _| {
        let tabs = tabs.downgrade();
        let sync = sync_attached.clone();
        gtk::glib::idle_add_local_once(move || {
            if let Some(tabs) = tabs.upgrade() {
                sync(&tabs);
            }
        });
    });
    let tabs_for_fullscreen = tabs.downgrade();
    let fullscreen_header = header.downgrade();
    let fullscreen_sidebar = sidebar.downgrade();
    let sidebar_toggle = toggle.downgrade();
    window.connect_fullscreened_notify(move |window| {
        let fullscreen = window.is_fullscreen();
        if let Some(tabs) = tabs_for_fullscreen.upgrade() {
            sync_toolbar(&tabs);
        }
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
    let weak_app = app.downgrade();
    let weak_tabs = tabs.downgrade();
    detach.connect_clicked(move |_| {
        if let (Some(app), Some(tabs)) = (weak_app.upgrade(), weak_tabs.upgrade()) {
            if let Some(page) = tabs.selected_page() {
                let (window, target) =
                    create_session_window(&app, state.clone(), Some(tabs.downgrade()));
                tabs.transfer_page(&page, &target, 0);
                window.present();
            }
        }
    });
    if let (Some(button), Some(origin)) = (return_tab, origin) {
        let source = tabs.downgrade();
        button.connect_clicked(move |button| {
            let Some(source) = source.upgrade() else {
                return;
            };
            let Some(target) = origin.upgrade() else {
                button.set_sensitive(false);
                return;
            };
            if let Some(page) = source.selected_page() {
                source.transfer_page(&page, &target, target.n_pages());
                target.set_selected_page(&page);
                if let Some(window) = target.root().and_downcast::<gtk::Window>() {
                    window.present();
                }
                if source.n_pages() == 0 {
                    if let Some(window) = source.root().and_downcast::<gtk::Window>() {
                        window.close();
                    }
                }
            }
        });
    }
    (window, tabs)
}
