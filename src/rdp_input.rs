use crate::rdp::{self, Control, Input};
use gtk::{gdk, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
};

fn special(key: gdk::Key) -> Option<u16> {
    use gdk::Key as K;
    Some(match key {
        K::Escape => 1,
        K::BackSpace => 14,
        K::Tab | K::ISO_Left_Tab => 15,
        K::Return => 28,
        K::KP_Enter => 0x11c,
        K::Control_L => 29,
        K::Control_R => 0x11d,
        K::Shift_L => 42,
        K::Shift_R => 54,
        K::Alt_L => 56,
        K::Alt_R | K::ISO_Level3_Shift => 0x138,
        K::Super_L | K::Meta_L => 0x15b,
        K::Super_R | K::Meta_R => 0x15c,
        K::Menu => 0x15d,
        K::Caps_Lock => 58,
        K::Num_Lock => 69,
        K::Scroll_Lock => 70,
        K::Home => 0x147,
        K::Up => 0x148,
        K::Page_Up => 0x149,
        K::Left => 0x14b,
        K::Right => 0x14d,
        K::End => 0x14f,
        K::Down => 0x150,
        K::Page_Down => 0x151,
        K::Insert => 0x152,
        K::Delete => 0x153,
        K::Print => 0x137,
        K::F1 => 59,
        K::F2 => 60,
        K::F3 => 61,
        K::F4 => 62,
        K::F5 => 63,
        K::F6 => 64,
        K::F7 => 65,
        K::F8 => 66,
        K::F9 => 67,
        K::F10 => 68,
        K::F11 => 87,
        K::F12 => 88,
        _ => return None,
    })
}
fn shortcut(key: gdk::Key) -> Option<u16> {
    let character = key.to_lower().to_unicode()?;
    for (row, start) in [
        ("1234567890-=", 2),
        ("qwertyuiop[]", 16),
        ("asdfghjkl;'", 30),
        ("zxcvbnm,./", 44),
    ] {
        if let Some(index) = row.chars().position(|c| c == character) {
            return Some(start + index as u16);
        }
    }
    match character {
        ' ' => Some(57),
        '\\' => Some(43),
        '`' => Some(41),
        _ => None,
    }
}
fn russian_scan(key: gdk::Key) -> Option<u16> {
    let character = key.to_lower().to_unicode()?;
    for (row, start) in [
        ("йцукенгшщзхъ", 16),
        ("фывапролджэ", 30),
        ("ячсмитьбю", 44),
        ("ё", 41),
    ] {
        if let Some(index) = row.chars().position(|c| c == character) {
            return Some(start + index as u16);
        }
    }
    None
}
// A missing backend map must not change the transport to Unicode.
fn physical_scan(key: gdk::Key, symbols: &[gdk::Key]) -> Option<u16> {
    special(key)
        .or_else(|| symbols.iter().find_map(|symbol| special(*symbol)))
        .or_else(|| symbols.iter().find_map(|symbol| shortcut(*symbol)))
        .or_else(|| symbols.iter().find_map(|symbol| russian_scan(*symbol)))
        .or_else(|| shortcut(key))
        .or_else(|| russian_scan(key))
}
fn stale_modifier(code: u16, state: gdk::ModifierType) -> bool {
    let mask = match code {
        29 | 0x11d => gdk::ModifierType::CONTROL_MASK,
        42 | 54 => gdk::ModifierType::SHIFT_MASK,
        56 => gdk::ModifierType::ALT_MASK,
        0x15b | 0x15c => gdk::ModifierType::SUPER_MASK | gdk::ModifierType::META_MASK,
        _ => return false,
    };
    !state.intersects(mask)
}
fn unicode(control: &Control, text: &str) {
    control.send_text(text);
}

fn point(picture: &gtk::Picture, x: f64, y: f64, size: (u32, u32)) -> Option<(u16, u16)> {
    rdp::remote_point(
        x,
        y,
        picture.width() as f64,
        picture.height() as f64,
        size.0,
        size.1,
    )
}

fn update_capture(
    picture: &gtk::Picture,
    captured: &Rc<RefCell<Option<gdk::Toplevel>>>,
    wanted: bool,
) {
    let previous = captured.borrow_mut().take();
    if let Some(top) = previous {
        top.restore_system_shortcuts();
    }
    let active = picture
        .root()
        .and_downcast::<gtk::Window>()
        .is_some_and(|window| window.is_active());
    if wanted && active && picture.is_mapped() && picture.is_sensitive() && picture.has_focus() {
        if let Some(top) = picture
            .native()
            .and_then(|native| native.surface())
            .and_then(|surface| surface.downcast::<gdk::Toplevel>().ok())
        {
            top.inhibit_system_shortcuts(None::<&gdk::Event>);
            *captured.borrow_mut() = Some(top);
        }
    }
}

pub fn attach(
    picture: &gtk::Picture,
    control: Arc<Control>,
    size: Rc<Cell<(u32, u32)>>,
    capture: &gtk::ToggleButton,
    text_mode: &gtk::CheckButton,
) {
    let captured: Rc<RefCell<Option<gdk::Toplevel>>> = Rc::new(RefCell::new(None));
    let weak = picture.downgrade();
    let grabbed = captured.clone();
    capture.connect_toggled(move |button| {
        if let Some(picture) = weak.upgrade() {
            if button.is_active() {
                picture.grab_focus();
            }
            update_capture(&picture, &grabbed, button.is_active());
        }
    });
    let text_mode = text_mode.downgrade();
    let capture_key = capture.downgrade();
    let pressed: Rc<RefCell<HashMap<u32, u16>>> = Rc::new(RefCell::new(HashMap::new()));
    let buttons: Rc<RefCell<HashMap<u32, (u32, u16)>>> = Rc::new(RefCell::new(HashMap::new()));
    let last = Rc::new(Cell::new((0u16, 0u16)));
    let inside = Rc::new(Cell::new(false));
    let im = gtk::IMMulticontext::new();
    im.set_client_widget(Some(picture));
    im.set_use_preedit(false);
    let sender = control.clone();
    let commit_mode = text_mode.clone();
    im.connect_commit(move |_, text| {
        if commit_mode
            .upgrade()
            .is_some_and(|button| button.is_active())
        {
            unicode(&sender, text);
        }
    });
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let sender = control.clone();
    let held = pressed.clone();
    let input = im.clone();
    let release_mode = text_mode.clone();
    keys.connect_key_pressed(move |controller, key, identity, state| {
        // Local layout shortcuts can consume key-up. Reconcile modifiers before
        // ordinary keys so an old remote Alt/Shift cannot poison subsequent text.
        if special(key).is_none() {
            held.borrow_mut().retain(|_, code| {
                if stale_modifier(*code, state) {
                    sender.send(Input::scan(*code, false, false));
                    false
                } else {
                    true
                }
            });
        }

        if key == gdk::Key::Control_R {
            if let Some(button) = capture_key.upgrade().filter(|b| b.is_active()) {
                button.set_active(false);
                for (_, code) in held.borrow_mut().drain() {
                    sender.send(Input::scan(code, false, false));
                }
                return glib::Propagation::Stop;
            }
        }
        if key == gdk::Key::Pause {
            sender.send(Input {
                kind: 4,
                flags: 0,
                a: 0,
                b: 0,
            });
            return glib::Propagation::Stop;
        }
        let raw = !text_mode.upgrade().is_some_and(|b| b.is_active());
        let modified = state.intersects(
            gdk::ModifierType::CONTROL_MASK
                | gdk::ModifierType::ALT_MASK
                | gdk::ModifierType::SUPER_MASK
                | gdk::ModifierType::META_MASK,
        );
        let symbols = controller
            .widget()
            .and_then(|widget| widget.display().map_keycode(identity))
            .unwrap_or_default()
            .into_iter()
            .map(|(_, symbol)| symbol)
            .collect::<Vec<_>>();
        let code = if raw || modified {
            physical_scan(key, &symbols)
        } else {
            special(key)
        };
        if let Some(code) = code {
            let repeat = held.borrow_mut().insert(identity, code).is_some();
            sender.send(Input::scan(code, true, repeat));
            return glib::Propagation::Stop;
        }
        if raw || modified {
            // Unknown physical keys stay local; never silently convert them to text.
            return glib::Propagation::Stop;
        }
        // Standard Cyrillic keyvals are already committed characters. Some IM
        // modules consume them without a commit on this custom drawing widget.
        if let Some(character) = key
            .to_unicode()
            .filter(|c| ('\u{0400}'..='\u{052f}').contains(c))
        {
            unicode(&sender, &character.to_string());
            return glib::Propagation::Stop;
        }
        if let Some(event) = controller.current_event() {
            if input.filter_keypress(&event) {
                return glib::Propagation::Stop;
            }
        }
        if let Some(character) = key.to_unicode().filter(|c| !c.is_control()) {
            unicode(&sender, &character.to_string());
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    let sender = control.clone();
    let held = pressed.clone();
    let input = im.clone();
    keys.connect_key_released(move |controller, _, identity, _| {
        if let Some(code) = held.borrow_mut().remove(&identity) {
            sender.send(Input::scan(code, false, false));
        } else if release_mode
            .upgrade()
            .is_some_and(|button| button.is_active())
        {
            if let Some(event) = controller.current_event() {
                input.filter_keypress(&event);
            }
        }
    });
    picture.add_controller(keys);
    let motion = gtk::EventControllerMotion::new();
    let weak = picture.downgrade();
    let sender = control.clone();
    let dimensions = size.clone();
    let last_point = last.clone();
    let in_picture = inside.clone();
    motion.connect_motion(move |_, x, y| {
        if let Some(picture) = weak.upgrade() {
            if let Some((x, y)) = point(&picture, x, y, dimensions.get()) {
                last_point.set((x, y));
                in_picture.set(true);
                sender.motion(x, y);
            } else {
                in_picture.set(false);
            }
        }
    });
    let in_picture = inside.clone();
    motion.connect_leave(move |_| in_picture.set(false));
    picture.add_controller(motion);
    let click = gtk::GestureClick::new();
    click.set_button(0);
    let weak = picture.downgrade();
    let sender = control.clone();
    let held_buttons = buttons.clone();
    let dimensions = size.clone();
    let last_point = last.clone();
    click.connect_pressed(move |gesture, _, x, y| {
        let Some(picture) = weak.upgrade() else {
            return;
        };
        let Some((x, y)) = point(&picture, x, y, dimensions.get()) else {
            return;
        };
        picture.grab_focus();
        last_point.set((x, y));
        let button = gesture.current_button();
        let (kind, flags) = match button {
            1 => (2, 0x1000),
            3 => (2, 0x2000),
            2 => (2, 0x4000),
            8 => (3, 1),
            9 => (3, 2),
            _ => return,
        };
        held_buttons.borrow_mut().insert(button, (kind, flags));
        sender.send(Input {
            kind,
            flags: flags | 0x8000,
            a: x,
            b: y,
        });
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    let sender = control.clone();
    let held_buttons = buttons.clone();
    let last_point = last.clone();
    click.connect_released(move |gesture, _, _, _| {
        if let Some((kind, flags)) = held_buttons.borrow_mut().remove(&gesture.current_button()) {
            let (x, y) = last_point.get();
            sender.send(Input {
                kind,
                flags,
                a: x,
                b: y,
            });
        }
    });
    let sender = control.clone();
    let held_buttons = buttons.clone();
    let last_point = last.clone();
    click.connect_cancel(move |_, _| {
        let (x, y) = last_point.get();
        for (_, (kind, flags)) in held_buttons.borrow_mut().drain() {
            sender.send(Input {
                kind,
                flags,
                a: x,
                b: y,
            });
        }
    });
    picture.add_controller(click);
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    let sender = control.clone();
    let last_point = last.clone();
    scroll.connect_scroll(move |_, dx, dy| {
        if !inside.get() {
            return glib::Propagation::Proceed;
        }
        let (x, y) = last_point.get();
        for (delta, flag) in [(-dy * 120.0, 0x200), (dx * 120.0, 0x400)] {
            let mut remaining = delta.round().clamp(-960.0, 960.0) as i16;
            while remaining != 0 {
                let step = remaining.clamp(-120, 120);
                sender.send(Input::mouse(flag | ((step as u16) & 0x1ff), x, y));
                remaining -= step;
            }
        }
        glib::Propagation::Stop
    });
    picture.add_controller(scroll);
    let focus = gtk::EventControllerFocus::new();
    let input = im.clone();
    let sender = control.clone();
    let refresh_capture = {
        let button = capture.downgrade();
        let picture = picture.downgrade();
        let captured = captured.clone();
        move || {
            let (Some(picture), Some(button)) = (picture.upgrade(), button.upgrade()) else {
                return;
            };
            update_capture(&picture, &captured, button.is_active());
        }
    };
    let refresh_on_focus = refresh_capture.clone();
    picture.connect_has_focus_notify(move |_| refresh_on_focus());
    let refresh_on_map = refresh_capture.clone();
    picture.connect_map(move |_| refresh_on_map());
    let grabbed_on_unmap = captured.clone();
    picture.connect_unmap(move |_| {
        if let Some(top) = grabbed_on_unmap.borrow_mut().take() {
            top.restore_system_shortcuts();
        }
    });
    let window_handler = RefCell::new(None::<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>);
    picture.connect_root_notify(move |picture| {
        if let Some((window, handler)) = window_handler.borrow_mut().take() {
            if let Some(window) = window.upgrade() {
                window.disconnect(handler);
            }
        }
        if let Some(window) = picture.root().and_downcast::<gtk::Window>() {
            let refresh = refresh_capture.clone();
            let handler = window.connect_is_active_notify(move |_| refresh());
            window_handler.replace(Some((window.downgrade(), handler)));
        }
        refresh_capture();
    });
    focus.connect_enter(move |_| {
        input.focus_in();
        sender.send(Input {
            kind: 5,
            flags: 0,
            a: 0,
            b: 0,
        });
    });
    let grabbed = captured.clone();
    picture.connect_unrealize(move |_| {
        if let Some(top) = grabbed.borrow_mut().take() {
            top.restore_system_shortcuts();
        }
    });
    let sender = control;
    focus.connect_leave(move |_| {
        if let Some(top) = captured.borrow_mut().take() {
            top.restore_system_shortcuts();
        }
        im.reset();
        im.focus_out();
        for (_, code) in pressed.borrow_mut().drain() {
            sender.send(Input::scan(code, false, false));
        }
        let (x, y) = last.get();
        for (_, (kind, flags)) in buttons.borrow_mut().drain() {
            sender.send(Input {
                kind,
                flags,
                a: x,
                b: y,
            });
        }
    });
    picture.add_controller(focus);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logical_key_mapping_does_not_depend_on_x11_hardware_codes() {
        assert!(stale_modifier(56, gdk::ModifierType::empty()));
        assert!(!stale_modifier(56, gdk::ModifierType::ALT_MASK));
        assert!(!stale_modifier(30, gdk::ModifierType::empty()));
        assert_eq!(special(gdk::Key::Control_R), Some(0x11d));
        assert_eq!(special(gdk::Key::Delete), Some(0x153));
        assert_eq!(shortcut(gdk::Key::C), Some(46));
        assert_eq!(shortcut(gdk::Key::F1), None);
        assert_eq!(russian_scan(gdk::Key::Cyrillic_ef), Some(30));
        assert_eq!(russian_scan(gdk::Key::Cyrillic_YA), Some(44));
        assert_eq!(russian_scan(gdk::Key::Cyrillic_io), Some(41));
        // Both empty GDK map and local group-switch keyvals must preserve scan input.
        assert_eq!(physical_scan(gdk::Key::Cyrillic_ef, &[]), Some(30));
        assert_eq!(physical_scan(gdk::Key::a, &[]), Some(30));
        assert_eq!(
            physical_scan(
                gdk::Key::ISO_Next_Group,
                &[gdk::Key::Alt_L, gdk::Key::ISO_Next_Group]
            ),
            Some(56)
        );
        assert_eq!(
            physical_scan(
                gdk::Key::ISO_Next_Group,
                &[gdk::Key::Shift_L, gdk::Key::ISO_Next_Group]
            ),
            Some(42)
        );
        assert_eq!(
            physical_scan(gdk::Key::Cyrillic_ef, &[gdk::Key::a, gdk::Key::Cyrillic_ef]),
            Some(30)
        );
    }
}
