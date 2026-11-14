use crate::{
    rdp::{self, CertificateDecision, Config, Event, Input},
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
    label.set_visible(true);
}
pub fn connect(
    parent: &adw::ApplicationWindow,
    tabs: &adw::TabView,
    state: Rc<State>,
    profile: Profile,
) {
    let dialog = adw::Dialog::builder()
        .title(format!("Подключиться: {}", profile.name))
        .content_width(560)
        .content_height(540)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.append(&adw::HeaderBar::new());
    let form = gtk::Box::new(gtk::Orientation::Vertical, 12);
    form.set_margin_start(16);
    form.set_margin_end(16);
    form.set_margin_bottom(16);
    let group=adw::PreferencesGroup::builder().description(format!("Сервер: {} · порт {}. Изменение пользователя/домена здесь действует только для этого сеанса.",profile.host,profile.port)).build();
    let user = adw::EntryRow::builder()
        .title("Пользователь")
        .text(&profile.username)
        .build();
    let domain = adw::EntryRow::builder()
        .title("Домен")
        .text(&profile.domain)
        .build();
    let password = adw::PasswordEntryRow::builder()
        .title("Пароль для этого подключения")
        .sensitive(false)
        .build();
    group.add(&user);
    group.add(&domain);
    group.add(&password);
    form.append(&group);
    let remember =
        gtk::CheckButton::with_label("Сохранить пароль на этом ПК для исходного профиля");
    form.append(&remember);
    let resolution = gtk::DropDown::from_strings(&["1280 × 720", "1600 × 900", "1920 × 1080"]);
    form.append(&resolution);
    let status = gtk::Label::builder()
        .label("Читаю локальное хранилище паролей…")
        .wrap(true)
        .xalign(0.0)
        .build();
    form.append(&status);
    let start = gtk::Button::with_label("Подключиться");
    start.add_css_class("suggested-action");
    start.set_sensitive(false);
    form.append(&start);
    user.set_sensitive(false);
    domain.set_sensitive(false);
    for entry in [&user, &domain] {
        let weak_user = user.downgrade();
        let weak_domain = domain.downgrade();
        let weak_password = password.downgrade();
        let weak_remember = remember.downgrade();
        let expected_user = profile.username.clone();
        let expected_domain = profile.domain.clone();
        entry.connect_changed(move |_| {
            if let (Some(user), Some(domain), Some(password), Some(remember)) = (
                weak_user.upgrade(),
                weak_domain.upgrade(),
                weak_password.upgrade(),
                weak_remember.upgrade(),
            ) {
                password.set_text("");
                let unchanged = user.text().as_str() == expected_user
                    && domain.text().as_str() == expected_domain;
                remember.set_sensitive(unchanged);
                if !unchanged {
                    remember.set_active(false);
                }
            }
        });
    }
    let closed = Rc::new(Cell::new(false));
    let flag = closed.clone();
    dialog.connect_closed(move |_| flag.set(true));
    let uid = profile.uid.clone();
    let weak_password = password.downgrade();
    let weak_start = start.downgrade();
    let weak_status = status.downgrade();
    let weak_user = user.downgrade();
    let weak_domain = domain.downgrade();
    glib::MainContext::default().spawn_local(async move{
        let result=secrets::lookup_rdp(&uid).await;if closed.get(){return;}
        if let(Some(password),Some(start),Some(status),Some(user),Some(domain))=(weak_password.upgrade(),weak_start.upgrade(),weak_status.upgrade(),weak_user.upgrade(),weak_domain.upgrade()){
            match result{
                Ok(Some(value))=>{password.set_text(&value);message(&status,"Используется пароль из локального хранилища.",true);}
                Ok(None)=>{status.remove_css_class("error");status.set_text("Введите пароль для этого сеанса. Он не попадёт в историю или наборы обмена.");}
                Err(error)=>message(&status,&format!("{error} Можно ввести пароль только для этого сеанса и не сохранять его."),false),
            }
            password.set_sensitive(true);user.set_sensitive(true);domain.set_sensitive(true);start.set_sensitive(true);
        }
    });
    let weak_dialog = dialog.downgrade();
    let weak_tabs = tabs.downgrade();
    let weak_root = root.downgrade();
    start.connect_clicked(move |_| {
        let (Some(dialog), Some(tabs), Some(root)) = (
            weak_dialog.upgrade(),
            weak_tabs.upgrade(),
            weak_root.upgrade(),
        ) else {
            return;
        };
        let username = user.text().trim().to_string();
        let domain = domain.text().trim().to_string();
        let secret = Zeroizing::new(password.text().to_string());
        let (width, height) = match resolution.selected() {
            1 => (1600, 900),
            2 => (1920, 1080),
            _ => (1280, 720),
        };
        let remember = remember.is_active();
        let state = state.clone();
        let profile = profile.clone();
        let status = status.clone();
        let password = password.clone();
        root.set_sensitive(false);
        dialog.set_can_close(false);
        glib::MainContext::default().spawn_local(async move {
            if remember {
                let Some(operation) = state.operation(&profile.uid) else {
                    message(
                        &status,
                        "Профиль изменяется в другом окне. Повторите позже.",
                        false,
                    );
                    root.set_sensitive(true);
                    dialog.set_can_close(true);
                    return;
                };
                let exists = state
                    .store
                    .profiles()
                    .ok()
                    .is_some_and(|profiles| profiles.iter().any(|p| p == &profile));
                if !exists {
                    message(
                        &status,
                        "Исходный профиль изменён или удалён. Откройте подключение заново.",
                        false,
                    );
                    root.set_sensitive(true);
                    dialog.set_can_close(true);
                    return;
                }
                if let Err(error) = secrets::store(&profile.uid, &secret).await {
                    message(&status, error, false);
                    root.set_sensitive(true);
                    dialog.set_can_close(true);
                    return;
                }
                drop(operation);
            }
            let mut history_profile = profile.clone();
            history_profile.username = username.clone();
            history_profile.domain = domain.clone();
            let config = Config {
                host: profile.host,
                port: profile.port,
                username,
                domain,
                password: secret,
                width,
                height,
            };
            open(&tabs, state, history_profile, config);
            password.set_text("");
            dialog.set_can_close(true);
            dialog.close();
        });
    });
    root.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&form)
            .build(),
    );
    dialog.set_child(Some(&root));
    dialog.present(Some(parent));
}

fn open(tabs: &adw::TabView, state: Rc<State>, profile: Profile, config: Config) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
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
    toolbar.append(&capture);
    let text_mode = gtk::CheckButton::with_label("Unicode");
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
    let sender = control.clone();
    disconnect.connect_clicked(move |button| {
        sender.stop();
        button.set_sensitive(false);
    });
    let sender = control.clone();
    cad.connect_clicked(move |_| {
        for (code, down) in [
            (29, true),
            (56, true),
            (0x153, true),
            (0x153, false),
            (56, false),
            (29, false),
        ] {
            sender.send(Input::scan(code, down, false));
        }
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
    let weak_page = page.downgrade();
    let changed_certificate = Cell::new(false);
    glib::timeout_add_local(Duration::from_millis(16), move || {
        let (Some(root), Some(picture), Some(status), Some(disconnect), Some(cad)) = (
            weak_root.upgrade(),
            weak_picture.upgrade(),
            weak_status.upgrade(),
            weak_disconnect.upgrade(),
            weak_cad.upgrade(),
        ) else {
            control.stop();
            return glib::ControlFlow::Break;
        };
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
        }
        while let Ok(event) = events.try_recv() {
            match event {
                Event::Connected => {
                    message(&status, "Подключено", true);
                    picture.set_sensitive(true);
                    cad.set_sensitive(true);
                    picture.grab_focus();
                    if let Some(id) = history {
                        let _ = state.store.history_connected(id);
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
