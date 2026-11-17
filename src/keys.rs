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
