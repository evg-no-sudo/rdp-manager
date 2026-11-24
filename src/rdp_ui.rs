use crate::{
    clipboard_files,
    rdp::{self, CertificateDecision, Config, Event},
    rdp_input, secrets,
    store::Profile,
    ui::State,
};
use adw::prelude::*;
use gtk::{gdk, glib};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
use zeroize::Zeroizing;

fn message(label: &gtk::Label, text: &str, success: bool) {
    label.remove_css_class("success");
    label.remove_css_class("error");
    label.add_css_class(if success { "success" } else { "error" });
    label.set_text(text);
    label.set_tooltip_text(Some(text));
    label.set_visible(true);
}
fn clipboard_has_text(clipboard: &gdk::Clipboard) -> bool {
    let formats = clipboard.formats();
    if [
        "text/uri-list",
        "x-special/gnome-copied-files",
        "application/x-gtk-file-list",
    ]
    .iter()
    .any(|mime| formats.contain_mime_type(mime))
    {
        return false;
    }
    formats.contains_type(glib::Type::STRING)
        || ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"]
            .iter()
            .any(|mime| formats.contain_mime_type(mime))
}
const REMOTE_FILES_MIME: &str = "application/x-rdp-manager-remote-files";
fn relay_files(
    clipboard: &gdk::Clipboard,
    picture: &gtk::Picture,
    source_marker: &str,
    sender: std::sync::Weak<rdp::Control>,
) {
    let formats = clipboard.formats();
    if !picture.has_focus()
        || !formats.contain_mime_type(REMOTE_FILES_MIME)
        || formats.contain_mime_type(source_marker)
    {
        return;
    }
    let Some(provider) = clipboard.content() else {
        return;
    };
    let clipboard_ref = clipboard.downgrade();
    let picture_ref = picture.downgrade();
    clipboard.read_value_async(
        gdk::FileList::static_type(),
        glib::Priority::DEFAULT,
        None::<&gtk::gio::Cancellable>,
        move |result| {
            let Ok(value) = result else {
                return;
            };
            let Ok(files) = value.get::<gdk::FileList>() else {
                return;
            };
            let Some(paths): Option<Vec<_>> =
                files.files().iter().map(|file| file.path()).collect()
            else {
                return;
            };
            glib::MainContext::default().spawn_local(async move {
                let Ok(Ok(entries)) =
                    gtk::gio::spawn_blocking(move || clipboard_files::snapshot(&paths)).await
                else {
                    return;
                };
                if picture_ref
                    .upgrade()
                    .is_some_and(|picture| picture.has_focus())
                    && clipboard_ref.upgrade().and_then(|cb| cb.content()).as_ref()
                        == Some(&provider)
                {
                    if let Some(sender) = sender.upgrade() {
                        sender.clipboard_files(entries);
                    }
                }
            });
        },
    );
}

fn offer_clipboard(
    clipboard: &gdk::Clipboard,
    sender: std::sync::Weak<rdp::Control>,
    expected: Rc<RefCell<Option<String>>>,
) {
    if clipboard.formats().contain_mime_type(REMOTE_FILES_MIME) {
        return;
    }
    if clipboard
        .formats()
        .contains_type(gdk::FileList::static_type())
    {
        clipboard.read_value_async(
            gdk::FileList::static_type(),
            glib::Priority::DEFAULT,
            None::<&gtk::gio::Cancellable>,
            move |result| {
                let Ok(value) = result else {
                    return;
                };
                let Ok(files) = value.get::<gdk::FileList>() else {
                    return;
                };
                let paths: Option<Vec<_>> = files.files().iter().map(|file| file.path()).collect();
                let Some(paths) = paths else {
                    return;
                };
                glib::MainContext::default().spawn_local(async move {
                    if let Ok(Ok(entries)) =
                        gtk::gio::spawn_blocking(move || clipboard_files::snapshot(&paths)).await
                    {
                        if let Some(sender) = sender.upgrade() {
                            sender.clipboard_files(entries);
                        }
                    }
                });
            },
        );
    } else if [
        "x-special/gnome-copied-files",
        "x-special/mate-copied-files",
        "text/uri-list",
    ]
    .iter()
    .any(|mime| clipboard.formats().contain_mime_type(mime))
    {
        clipboard.read_async(
            &[
                "x-special/gnome-copied-files",
                "x-special/mate-copied-files",
                "text/uri-list",
            ],
            glib::Priority::DEFAULT,
            None::<&gtk::gio::Cancellable>,
            move |result| {
                let Ok((stream, _)) = result else {
                    return;
                };
                glib::MainContext::default().spawn_local(async move {
                    let Ok(bytes) = stream
                        .read_bytes_future(1024 * 1024, glib::Priority::DEFAULT)
                        .await
                    else {
                        return;
                    };
                    let Ok(text) = std::str::from_utf8(bytes.as_ref()) else {
                        return;
                    };
                    let paths: Vec<_> = text
                        .lines()
                        .filter(|line| line.starts_with("file://"))
                        .filter_map(|line| gtk::gio::File::for_uri(line).path())
                        .collect();
                    if paths.is_empty() {
                        return;
                    }
                    if let Ok(Ok(entries)) =
                        gtk::gio::spawn_blocking(move || clipboard_files::snapshot(&paths)).await
                    {
                        if let Some(sender) = sender.upgrade() {
                            sender.clipboard_files(entries);
                        }
                    }
                });
            },
        );
    } else if clipboard_has_text(clipboard) {
        clipboard.read_text_async(None::<&gtk::gio::Cancellable>, move |result| {
            let Ok(Some(text)) = result else {
                return;
            };
            let text = text.to_string();
            if expected.borrow().as_ref() == Some(&text) {
                return;
            }
            expected.borrow_mut().take();
            if let Some(sender) = sender.upgrade() {
                sender.clipboard_text(text);
            }
        });
    }
}
fn connect_saved(
    parent: &adw::ApplicationWindow,
    tabs: &adw::TabView,
    state: Rc<State>,
    uid: &str,
    secret: Zeroizing<String>,
) {
    let result = (|| {
        let profile = state
            .store
            .profiles()?
            .into_iter()
            .find(|p| p.uid == uid)
            .ok_or("Подключение удалено")?;
        let options = state.store.session_options(uid)?;
        let shared_folder = if options.shared_folder.is_empty() {
            String::new()
        } else {
            let path = std::fs::canonicalize(&options.shared_folder)
                .map_err(|_| "Общая папка недоступна. Проверьте путь в настройках профиля.")?;
            if !path.is_dir() {
                return Err("Общая папка должна быть каталогом.".into());
            }
            path.to_string_lossy().into_owned()
        };
        let (width, height) = match options.resolution {
            1 => (1280, 720),
            2 => (1600, 900),
            3 => (1920, 1080),
            _ => rdp::desktop_size(tabs.width(), tabs.height() - 60, tabs.scale_factor())
                .map(|(w, h)| (u32::from(w), u32::from(h)))
                .unwrap_or((1280, 720)),
        };
        let config = Config {
            host: profile.host.clone(),
            port: profile.port,
            username: profile.username.clone(),
            domain: profile.domain.clone(),
            password: secret,
            shared_folder,
            width,
            height,
            keyboard_layout: options.keyboard_layout,
            quality: options.quality,
            auto_resize: options.resolution == 0,
        };
        Ok::<_, Box<dyn std::error::Error>>((profile, config))
    })();
    match result {
        Ok((profile, config)) => open(tabs, state, profile, config),
        Err(error) => {
            let alert = adw::AlertDialog::builder()
                .heading("Не удалось подключиться")
                .body(error.to_string())
                .build();
            alert.add_response("close", "Закрыть");
            alert.present(Some(parent));
        }
    }
}

pub fn connect(
    parent: &adw::ApplicationWindow,
    tabs: &adw::TabView,
    state: Rc<State>,
    profile: Profile,
) {
    let Some(operation) = state.operation(&profile.uid) else {
        return;
    };
    let weak_parent = parent.downgrade();
    let weak_tabs = tabs.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let result = secrets::lookup_rdp(&profile.uid).await;
        drop(operation);
        let (Some(parent), Some(tabs)) = (weak_parent.upgrade(), weak_tabs.upgrade()) else {
            return;
        };
        if let Ok(Some(secret)) = result {
            connect_saved(&parent, &tabs, state, &profile.uid, secret);
            return;
        }
        let account = if profile.username.is_empty() {
            "Не указан в профиле".to_owned()
        } else if profile.domain.is_empty() {
            profile.username.clone()
        } else {
            format!("{}\\{}", profile.domain, profile.username)
        };
        let prompt = adw::AlertDialog::builder()
            .heading(format!("Пароль: {}", profile.name))
            .body(format!(
                "Пользователь: {account}\nВведите пароль для подключения."
            ))
            .build();
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let password = gtk::PasswordEntry::builder().show_peek_icon(true).build();
        let remember = gtk::CheckButton::with_label("Сохранить пароль на этом ПК");
        fields.append(&password);
        fields.append(&remember);
        prompt.set_extra_child(Some(&fields));
        prompt.add_responses(&[("cancel", "Отмена"), ("connect", "Подключиться")]);
        prompt.set_close_response("cancel");
        prompt.set_default_response(Some("connect"));
        let weak_parent = parent.downgrade();
        let weak_tabs = tabs.downgrade();
        let focus_password = password.clone();
        prompt.connect_response(None, move |_, response| {
            if response != "connect" {
                password.set_text("");
                return;
            }
            let secret = Zeroizing::new(password.text().to_string());
            password.set_text("");
            let remember = remember.is_active();
            let state = state.clone();
            let uid = profile.uid.clone();
            let weak_parent = weak_parent.clone();
            let weak_tabs = weak_tabs.clone();
            glib::MainContext::default().spawn_local(async move {
                let Some(operation) = state.operation(&uid) else {
                    return;
                };
                if !state
                    .store
                    .profiles()
                    .ok()
                    .is_some_and(|profiles| profiles.iter().any(|p| p.uid == uid))
                {
                    return;
                }
                if remember {
                    if let Err(error) = secrets::store(&uid, &secret).await {
                        if let Some(parent) = weak_parent.upgrade() {
                            let alert = adw::AlertDialog::builder()
                                .heading("Пароль не сохранён")
                                .body(error)
                                .build();
                            alert.add_response("close", "Закрыть");
                            alert.present(Some(&parent));
                        }
                        return;
                    }
                }
                drop(operation);
                if let (Some(parent), Some(tabs)) = (weak_parent.upgrade(), weak_tabs.upgrade()) {
                    connect_saved(&parent, &tabs, state, &uid, secret);
                }
            });
        });
        prompt.present(Some(&parent));
        focus_password.grab_focus();
    });
}

fn toggle_fullscreen(widget: &impl IsA<gtk::Widget>) {
    if let Some(window) = widget.root().and_downcast::<gtk::Window>() {
        if window.is_fullscreen() {
            window.unfullscreen();
        } else {
            window.fullscreen();
        }
    }
}

pub(crate) const SESSION_TOOLBAR: &str = "rdp-session-toolbar";

fn open(tabs: &adw::TabView, state: Rc<State>, profile: Profile, config: Config) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    toolbar.set_widget_name(SESSION_TOOLBAR);
    toolbar.set_margin_start(12);
    toolbar.set_margin_end(12);
    let status = gtk::Label::builder()
        .label("Подключение…")
        .hexpand(true)
        .xalign(0.0)
        .wrap(true)
        .build();
    toolbar.append(&status);
    let cad = gtk::Button::with_label("Ctrl+Alt+Del");
    cad.set_sensitive(false);
    toolbar.append(&cad);
    let capture = gtk::ToggleButton::with_label("Захват клавиш");
    capture.set_tooltip_text(Some("Системные сочетания — удалённому ПК. Правый Ctrl освобождает клавиатуру. В Wayland требуется поддержка композитора."));
    capture.set_focus_on_click(false);
    capture.set_sensitive(false);
    toolbar.append(&capture);
    let fullscreen = gtk::Button::from_icon_name("view-fullscreen-symbolic");
    fullscreen.set_tooltip_text(Some("Полный экран / выход (Ctrl+Shift+F11)"));
    fullscreen.set_focus_on_click(false);
    fullscreen.connect_clicked(toggle_fullscreen);
    toolbar.append(&fullscreen);
    let window_handler = RefCell::new(None::<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>);
    let weak_button = fullscreen.downgrade();
    root.connect_root_notify(move |root| {
        if let Some((window, handler)) = window_handler.borrow_mut().take() {
            if let Some(window) = window.upgrade() {
                window.disconnect(handler);
            }
        }
        if let (Some(window), Some(button)) = (
            root.root().and_downcast::<gtk::Window>(),
            weak_button.upgrade(),
        ) {
            let update = |window: &gtk::Window, button: &gtk::Button| {
                let (icon, tooltip) = if window.is_fullscreen() {
                    (
                        "view-restore-symbolic",
                        "Выйти из полного экрана (Ctrl+Shift+F11)",
                    )
                } else {
                    ("view-fullscreen-symbolic", "Полный экран (Ctrl+Shift+F11)")
                };
                button.set_icon_name(icon);
                button.set_tooltip_text(Some(tooltip));
            };
            update(&window, &button);
            let weak_button = button.downgrade();
            let handler = window.connect_fullscreened_notify(move |window| {
                if let Some(button) = weak_button.upgrade() {
                    update(window, &button);
                }
            });
            window_handler.replace(Some((window.downgrade(), handler)));
        }
    });

    let fullscreen_keys = gtk::EventControllerKey::new();
    fullscreen_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let fullscreen_down = Rc::new(Cell::new(false));
    let pressed = fullscreen_down.clone();
    fullscreen_keys.connect_key_pressed(move |controller, key, _, modifiers| {
        let chord = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
        if key == gdk::Key::F11 && (pressed.get() || modifiers.contains(chord)) {
            if !pressed.replace(true) {
                if let Some(widget) = controller.widget() {
                    toggle_fullscreen(&widget);
                }
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    fullscreen_keys.connect_key_released(move |_, key, _, _| {
        if key == gdk::Key::F11 {
            fullscreen_down.set(false);
        }
    });
    root.add_controller(fullscreen_keys);

    let text_mode = gtk::CheckButton::with_label("Текст Unicode по локальной раскладке");
    let keyboard_mode = gtk::Label::new(Some("Раскладка: сервер"));
    keyboard_mode.add_css_class("dim-label");
    toolbar.append(&keyboard_mode);
    text_mode.connect_toggled(move |button| {
        keyboard_mode.set_label(if button.is_active() {
            "Раскладка: локальная"
        } else {
            "Раскладка: сервер"
        });
    });
    text_mode.set_tooltip_text(Some("Передача текста по локальной раскладке. Для xrdp оставьте выключенным и переключайте раскладку на удалённом ПК."));
    let options = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Параметры ввода")
        .build();
    let popover = gtk::Popover::new();
    let options_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    options_box.set_margin_top(12);
    options_box.set_margin_bottom(12);
    options_box.set_margin_start(12);
    options_box.set_margin_end(12);
    options_box.append(&text_mode);
    popover.set_child(Some(&options_box));
    options.set_popover(Some(&popover));
    toolbar.append(&options);
    let disconnect = gtk::Button::with_label("Отключить");
    toolbar.append(&disconnect);
    root.append(&toolbar);
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .hexpand(true)
        .vexpand(true)
        .focusable(true)
        .can_target(true)
        .sensitive(false)
        .build();
    root.append(&picture);
    let page = tabs.append(&root);
    page.set_title(&profile.name);
    tabs.set_selected_page(&page);
    let history = match state.store.history_start(&profile) {
        Ok(id) => Some(id),
        Err(_) => {
            message(&status, "Подключение… (историю записать не удалось)", false);
            None
        }
    };
    let initial_size = (config.width, config.height);
    let auto_resize = config.auto_resize;
    let (control, events) = match rdp::start(config) {
        Ok(session) => session,
        Err(error) => {
            message(&status, &error, false);
            disconnect.set_sensitive(false);
            if let Some(id) = history {
                let _ = state.store.history_finish(id, &error, 0);
            }
            return;
        }
    };
    let size = Rc::new(Cell::new(initial_size));
    let pending_certificate: Rc<RefCell<Option<adw::AlertDialog>>> = Rc::new(RefCell::new(None));
    let weak_control = std::sync::Arc::downgrade(&control);
    let weak_prompt = Rc::downgrade(&pending_certificate);
    let weak_state = Rc::downgrade(&state);
    state.register_session(
        &root,
        Rc::new(move || {
            if let Some(control) = weak_control.upgrade() {
                control.stop();
            }
            if let Some(prompt) = weak_prompt.upgrade() {
                let alert = prompt.borrow_mut().take();
                if let Some(alert) = alert {
                    alert.close();
                }
            }
            if let (Some(state), Some(id)) = (weak_state.upgrade(), history) {
                let _ = state.store.history_finish(id, "Закрыто пользователем", 0);
            }
        }),
    );
    rdp_input::attach(
        &picture,
        control.clone(),
        size.clone(),
        &capture,
        &text_mode,
    );
    let clipboard = picture.display().clipboard();
    let expected_remote_text = Rc::new(RefCell::new(None::<String>));
    let weak_sender = std::sync::Arc::downgrade(&control);
    let source_marker = format!("{REMOTE_FILES_MIME}-{}", glib::uuid_string_random());
    let marker_on_change = source_marker.clone();
    let expected_on_change = expected_remote_text.clone();
    let picture_on_change = picture.downgrade();
    let sender_on_change = weak_sender.clone();
    let clipboard_handler = clipboard.connect_changed(move |clipboard| {
        if clipboard.formats().contain_mime_type(REMOTE_FILES_MIME) {
            if let Some(picture) = picture_on_change.upgrade() {
                relay_files(
                    clipboard,
                    &picture,
                    &marker_on_change,
                    sender_on_change.clone(),
                );
            }
        } else {
            offer_clipboard(
                clipboard,
                sender_on_change.clone(),
                expected_on_change.clone(),
            );
        }
    });
    let clipboard_on_focus = clipboard.clone();
    let marker_on_focus = source_marker.clone();
    picture.connect_has_focus_notify(move |picture| {
        relay_files(
            &clipboard_on_focus,
            picture,
            &marker_on_focus,
            weak_sender.clone(),
        );
    });
    let clipboard_on_close = clipboard.clone();
    let clipboard_handler = RefCell::new(Some(clipboard_handler));
    root.connect_destroy(move |_| {
        if let Some(handler) = clipboard_handler.borrow_mut().take() {
            clipboard_on_close.disconnect(handler);
        }
    });
    options_box.append(&gtk::Label::new(Some("Переключить раскладку на сервере")));
    for (label, codes) in [
        ("Alt+Shift", vec![56, 42]),
        ("Ctrl+Shift", vec![29, 42]),
        ("Win+Space", vec![0x15b, 57]),
    ] {
        let button = gtk::Button::with_label(label);
        button.set_tooltip_text(Some(
            "Сработает, если это сочетание настроено на удалённом ПК",
        ));
        let sender = control.clone();
        let weak_picture = picture.downgrade();
        let weak_popover = popover.downgrade();
        button.connect_clicked(move |_| {
            if let Some(popover) = weak_popover.upgrade() {
                popover.popdown();
            }
            if let Some(picture) = weak_picture.upgrade() {
                picture.grab_focus();
            }
            sender.chord(&codes);
        });
        options_box.append(&button);
    }
    let test_text = gtk::Button::with_label("Отправить «Привет»");
    test_text.set_tooltip_text(Some(
        "Проверка Unicode: заранее откройте текстовый редактор на удалённом ПК",
    ));
    let sender = control.clone();
    let weak_picture = picture.downgrade();
    let weak_popover = popover.downgrade();
    test_text.connect_clicked(move |_| {
        if let Some(popover) = weak_popover.upgrade() {
            popover.popdown();
        }
        if let Some(picture) = weak_picture.upgrade() {
            picture.grab_focus();
        }
        sender.send_text("Привет");
    });
    options_box.append(&test_text);
    let sender = control.clone();
    let rendering = Rc::new(Cell::new(true));
    let rendering_on_disconnect = rendering.clone();
    let picture_on_disconnect = picture.downgrade();
    disconnect.connect_clicked(move |button| {
        rendering_on_disconnect.set(false);
        if let Some(picture) = picture_on_disconnect.upgrade() {
            picture.set_paintable(None::<&gdk::Paintable>);
            picture.set_sensitive(false);
        }
        sender.stop();
        button.set_sensitive(false);
    });
    let sender = control.clone();
    cad.connect_clicked(move |_| {
        sender.chord(&[29, 56, 0x153]);
    });
    let sender = control.clone();
    let state_on_close = state.clone();
    root.connect_destroy(move |_| {
        sender.stop();
        if let Some(id) = history {
            let _ = state_on_close
                .store
                .history_finish(id, "Закрыто пользователем", 0);
        }
    });
    let weak_root = root.downgrade();
    let weak_picture = picture.downgrade();
    let weak_status = status.downgrade();
    let weak_disconnect = disconnect.downgrade();
    let weak_cad = cad.downgrade();
    let weak_capture = capture.downgrade();
    let weak_page = page.downgrade();
    let mut remote_file_provider = None::<gdk::ContentProvider>;
    let session_clipboard = clipboard.clone();
    let clipboard_sender = std::sync::Arc::downgrade(&control);
    let expected_on_connect = expected_remote_text.clone();
    let changed_certificate = Cell::new(false);
    let frame_control = control.clone();
    let trace_graphics = std::env::var_os("RDP_MANAGER_TRACE_GRAPHICS").is_some();
    let shown = Cell::new(0u64);
    let last_trace = Cell::new(std::time::Instant::now());
    let measured = Cell::new(None);
    let requested = Cell::new(None);
    let changed_at = Cell::new(std::time::Instant::now());
    let rendering_on_tick = rendering.clone();
    picture.add_tick_callback(move |picture, _| {
        if !rendering_on_tick.get() {
            return glib::ControlFlow::Break;
        }
        if auto_resize {
            let dimensions =
                rdp::desktop_size(picture.width(), picture.height(), picture.scale_factor());
            if dimensions != measured.get() {
                measured.set(dimensions);
                changed_at.set(std::time::Instant::now());
            }
            if let Some((w, h)) = dimensions.filter(|value| {
                Some(*value) != requested.get()
                    && changed_at.get().elapsed() >= Duration::from_millis(200)
            }) {
                frame_control.resize(w, h);
                requested.set(dimensions);
            }
        }
        let control = &frame_control;
        if let Some(frame) = control.take_frame() {
            size.set((frame.width, frame.height));
            let bytes = glib::Bytes::from_owned(frame.data);
            let texture = gdk::MemoryTexture::new(
                frame.width as i32,
                frame.height as i32,
                gdk::MemoryFormat::B8g8r8a8Premultiplied,
                &bytes,
                frame.width as usize * 4,
            );
            picture.set_paintable(Some(&texture));
            shown.set(shown.get() + 1);
        }
        if trace_graphics && last_trace.get().elapsed() >= Duration::from_secs(3) {
            let elapsed = last_trace.get().elapsed().as_secs_f64();
            let renderer = picture
                .native()
                .and_then(|native| native.renderer())
                .map(|renderer| renderer.type_().name().to_string())
                .unwrap_or_else(|| "unavailable".into());
            eprintln!(
                "[rdp-present] renderer={renderer} texture_updates_per_sec={:.1}",
                shown.get() as f64 / elapsed
            );
            shown.set(0);
            last_trace.set(std::time::Instant::now());
        }
        glib::ControlFlow::Continue
    });
    glib::timeout_add_local(Duration::from_millis(16), move || {
        let (Some(root), Some(picture), Some(status), Some(disconnect), Some(cad), Some(capture)) = (
            weak_root.upgrade(),
            weak_picture.upgrade(),
            weak_status.upgrade(),
            weak_disconnect.upgrade(),
            weak_cad.upgrade(),
            weak_capture.upgrade(),
        ) else {
            control.stop();
            return glib::ControlFlow::Break;
        };
        while let Ok(event) = events.try_recv() {
            match event {
                Event::Connected => {
                    message(&status, "Подключено", true);
                    picture.set_sensitive(true);
                    cad.set_sensitive(true);
                    picture.grab_focus();
                    capture.set_sensitive(true);
                    capture.set_active(true);
                    let sender = clipboard_sender.clone();
                    let expected = expected_on_connect.clone();
                    offer_clipboard(&session_clipboard, sender, expected);
                    if let Some(id) = history {
                        let _ = state.store.history_connected(id);
                    }
                }
                Event::Clipboard(text) => {
                    expected_remote_text.replace(Some(text.clone()));
                    session_clipboard.set_text(&text);
                }
                Event::ClipboardFiles(paths) => {
                    let files: Vec<_> = paths
                        .lines()
                        .filter(|line| line.starts_with('/'))
                        .map(gtk::gio::File::for_path)
                        .collect();
                    if files.is_empty() {
                        if remote_file_provider.take().is_some_and(|provider| {
                            session_clipboard.content().as_ref() == Some(&provider)
                        }) {
                            let _ = session_clipboard.set_content(None::<&gdk::ContentProvider>);
                        }
                    } else {
                        let urls: Vec<_> = files.iter().map(|file| file.uri()).collect();
                        let urls: Vec<_> = urls.iter().map(|uri| uri.as_str()).collect();
                        let uri_list = format!("{}\r\n", urls.join("\r\n"));
                        let copied_files = format!("copy\n{}", urls.join("\n"));
                        let provider = gdk::ContentProvider::new_union(&[
                            gdk::ContentProvider::for_value(
                                &gdk::FileList::from_array(&files).to_value(),
                            ),
                            gdk::ContentProvider::for_bytes(
                                "text/uri-list",
                                &glib::Bytes::from_owned(uri_list),
                            ),
                            gdk::ContentProvider::for_bytes(
                                "x-special/gnome-copied-files",
                                &glib::Bytes::from_owned(copied_files.clone()),
                            ),
                            gdk::ContentProvider::for_bytes(
                                "x-special/mate-copied-files",
                                &glib::Bytes::from_owned(copied_files),
                            ),
                            gdk::ContentProvider::for_bytes(
                                REMOTE_FILES_MIME,
                                &glib::Bytes::from_static(b""),
                            ),
                            gdk::ContentProvider::for_bytes(
                                &source_marker,
                                &glib::Bytes::from_static(b""),
                            ),
                        ]);
                        if session_clipboard.set_content(Some(&provider)).is_ok() {
                            remote_file_provider = Some(provider);
                            if std::env::var_os("RDP_MANAGER_TRACE_CLIPBOARD").is_some() {
                                eprintln!("[rdp-clipboard] published {} remote items", files.len());
                            }
                        }
                    }
                }
                Event::Certificate(cert, reply) => {
                    if cert.changed {
                        changed_certificate.set(true);
                        message(
                            &status,
                            "Сертификат RDP-сервера изменился. Подключение отклонено.",
                            false,
                        );
                        let alert=adw::AlertDialog::builder().heading("Сертификат сервера изменился")
                        .body(format!("{}:{}\nПредыдущий отпечаток: {}\nНовый отпечаток: {}\nПодключение остановлено. Проверьте замену сертификата у администратора.",cert.host,cert.port,cert.old_fingerprint,cert.fingerprint)).build();
                        alert.add_response("close", "Закрыть");
                        alert.present(Some(&root));
                        let _ = reply.send(CertificateDecision::Reject);
                    } else {
                        let alert=adw::AlertDialog::builder().heading("Проверьте сертификат RDP-сервера")
                        .body(format!("{}:{}\nИмя: {}\nСубъект: {}\nИздатель: {}\nSHA256: {}\n\nСверьте отпечаток с администратором. Разрешить подключение?",cert.host,cert.port,cert.common,cert.subject,cert.issuer,cert.fingerprint)).build();
                        alert.add_responses(&[("cancel", "Отмена"), ("accept", "Подключиться")]);
                        alert.set_default_response(Some("cancel"));
                        alert.set_close_response("cancel");
                        let remember =
                            gtk::CheckButton::with_label("Запомнить сертификат для этого сервера");
                        alert.set_extra_child(Some(&remember));
                        alert.connect_response(None, move |_, response| {
                            let decision = if response != "accept" {
                                CertificateDecision::Reject
                            } else if remember.is_active() {
                                CertificateDecision::Remember
                            } else {
                                CertificateDecision::Session
                            };
                            let _ = reply.send(decision);
                        });
                        pending_certificate.replace(Some(alert.clone()));
                        alert.present(Some(&root));
                    }
                }
                Event::Ended {
                    code,
                    message: text,
                    cancelled,
                } => {
                    if let Some(alert) = pending_certificate.borrow_mut().take() {
                        alert.close();
                    }
                    let text = if changed_certificate.get() {
                        "Подключение отклонено: сертификат RDP-сервера изменился".to_string()
                    } else if cancelled {
                        "Отключено пользователем".to_string()
                    } else {
                        text
                    };
                    message(
                        &status,
                        &text,
                        cancelled || (code == 0 && text == "Сеанс завершён"),
                    );
                    disconnect.set_sensitive(false);
                    cad.set_sensitive(false);
                    capture.set_active(false);
                    capture.set_sensitive(false);
                    rendering.set(false);
                    picture.set_paintable(None::<&gdk::Paintable>);
                    picture.set_sensitive(false);
                    if let Some(page) = weak_page.upgrade() {
                        page.set_title(&format!("{} — завершён", profile.name));
                    }
                    if let Some(id) = history {
                        let _ = state.store.history_finish(id, &text, code);
                    }
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

pub fn history(parent: &adw::ApplicationWindow, state: Rc<State>) {
    let dialog = adw::Dialog::builder()
        .title("История подключений")
        .content_width(700)
        .content_height(600)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    root.append(&adw::HeaderBar::new());
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_valign(gtk::Align::Start);
    list.add_css_class("boxed-list");
    match state.store.history() {
        Ok(entries) if entries.is_empty() => {
            list.append(&gtk::Label::new(Some("История пока пуста")))
        }
        Ok(entries) => {
            for entry in entries {
                let time = glib::DateTime::from_unix_utc(entry.started)
                    .and_then(|time| time.to_local())
                    .and_then(|time| time.format("%d.%m.%Y %H:%M:%S"))
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let outcome = if entry.ended.is_none() {
                    if entry.connected {
                        "Подключено"
                    } else {
                        "Подключение…"
                    }
                } else {
                    &entry.outcome
                };
                let row = adw::ActionRow::builder()
                    .title(&entry.name)
                    .subtitle(format!(
                        "{} · порт {} · {}\n{time} · {outcome}",
                        entry.host, entry.port, entry.username
                    ))
                    .build();
                row.set_use_markup(false);
                list.append(&row);
            }
        }
        Err(_) => list.append(&gtk::Label::new(Some("Не удалось прочитать историю"))),
    }
    root.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build(),
    );
    dialog.set_child(Some(&root));
    dialog.present(Some(parent));
}
