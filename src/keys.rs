//! Übersetzt winit-Tastaturereignisse in Servos (W3C-)Tastaturereignisse.
//!
//! winit und keyboard-types benennen Tasten beide nach der W3C-Spezifikation ("Enter",
//! "ArrowLeft", "KeyA"), daher genügt meist der Name.

use servo::{Code, Key, KeyState, KeyboardEvent, Location, Modifiers, NamedKey};
use winit::event::{ElementState, KeyEvent};
use winit::keyboard::{Key as WinitKey, KeyLocation, ModifiersState, NamedKey as WinitNamed, PhysicalKey};

pub fn to_servo(event: &KeyEvent, modifiers: ModifiersState) -> KeyboardEvent {
    let state = match event.state {
        ElementState::Pressed => KeyState::Down,
        ElementState::Released => KeyState::Up,
    };
    let key = match &event.logical_key {
        WinitKey::Named(WinitNamed::Space) => Key::Character(" ".into()),
        WinitKey::Character(s) => Key::Character(s.to_string()),
        WinitKey::Named(named) => format!("{named:?}")
            .parse::<NamedKey>()
            .map(Key::Named)
            .unwrap_or(Key::Named(NamedKey::Unidentified)),
        _ => Key::Named(NamedKey::Unidentified),
    };
    let code = match event.physical_key {
        PhysicalKey::Code(code) => physical_name(&format!("{code:?}"))
            .parse::<Code>()
            .unwrap_or(Code::Unidentified),
        PhysicalKey::Unidentified(_) => Code::Unidentified,
    };
    let location = match event.location {
        KeyLocation::Standard => Location::Standard,
        KeyLocation::Left => Location::Left,
        KeyLocation::Right => Location::Right,
        KeyLocation::Numpad => Location::Numpad,
    };
    let mut mods = Modifiers::empty();
    if modifiers.shift_key() {
        mods |= Modifiers::SHIFT;
    }
    if modifiers.control_key() {
        mods |= Modifiers::CONTROL;
    }
    if modifiers.alt_key() {
        mods |= Modifiers::ALT;
    }
    if modifiers.super_key() {
        mods |= Modifiers::META;
    }
    KeyboardEvent::new_without_event(state, key, code, location, mods, event.repeat, false)
}

/// Die wenigen Stellen, an denen winit andere Namen als die W3C-Codes benutzt.
fn physical_name(name: &str) -> &str {
    match name {
        "SuperLeft" => "MetaLeft",
        "SuperRight" => "MetaRight",
        other => other,
    }
}

/// Ist das ein Tastenkürzel des Browsers? Muss zu `Browser::shortcut` in main.rs passen.
/// WebView2-Tabs fangen damit die Kürzel ab, bevor die Seite sie bekommt.
pub fn is_browser_shortcut(key: &winit::keyboard::Key, mods: ModifiersState) -> bool {
    use winit::keyboard::Key as K;
    let (ctrl, alt, shift) = (mods.control_key(), mods.alt_key(), mods.shift_key());
    match key {
        K::Character(c) if ctrl => {
            let c = c.to_ascii_lowercase();
            matches!(c.as_str(), "l" | "r" | "t" | "w" | "b" | "d" | "h" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                || (shift && (c == "e" || c == "z"))
        }
        K::Named(WinitNamed::Tab) => ctrl,
        K::Named(WinitNamed::Delete) => ctrl && shift,
        K::Named(WinitNamed::ArrowLeft | WinitNamed::ArrowRight) => alt,
        K::Named(WinitNamed::F2 | WinitNamed::F3 | WinitNamed::F5 | WinitNamed::F6) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::{Key, SmolStr};

    #[test]
    fn browser_kuerzel() {
        let ch = |c: &str| Key::Character(SmolStr::new(c));
        assert!(is_browser_shortcut(&ch("t"), ModifiersState::CONTROL));
        assert!(is_browser_shortcut(&ch("E"), ModifiersState::CONTROL | ModifiersState::SHIFT));
        assert!(!is_browser_shortcut(&ch("e"), ModifiersState::CONTROL));
        assert!(!is_browser_shortcut(&ch("c"), ModifiersState::CONTROL));
        assert!(!is_browser_shortcut(&ch("t"), ModifiersState::empty()));
        assert!(is_browser_shortcut(&Key::Named(WinitNamed::F5), ModifiersState::empty()));
    }
}
