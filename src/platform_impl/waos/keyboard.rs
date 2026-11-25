//! A view's key events as winit's.
//!
//! WaOS sends each key as Flutter's `KeyData`: where the key is, which key the layout makes it,
//! and the character it types. It does not say which modifiers are held, so they are counted
//! here from the modifier keys as they go down and up.

use smol_str::SmolStr;
use wasdk::keys::logical;
use wasdk::views::{KeyEvent as ViewKeyEvent, KeyKind};

use super::key_names::{key_code, named_key};
use super::KeyEventExtra;
use crate::event::{ElementState, KeyEvent, Modifiers};
use crate::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersKeys, NativeKey, NativeKeyCode, PhysicalKey,
};

/// The modifier keys held in one window.
#[derive(Default)]
pub(super) struct Keyboard {
    held: ModifiersKeys,
}

impl Keyboard {
    /// `event` as winit's, with the modifiers held after it where it changed them.
    pub(super) fn translate(&mut self, event: &ViewKeyEvent) -> (KeyEvent, Option<Modifiers>) {
        let code = key_code(event.physical);
        let pressed = event.kind != KeyKind::Up;
        let modifiers = modifier_key(event.logical).and_then(|key| self.hold(key, pressed));
        let key_event = KeyEvent {
            physical_key: match code {
                Some(code) => PhysicalKey::Code(code),
                None => PhysicalKey::Unidentified(NativeKeyCode::Unidentified),
            },
            logical_key: logical_key(event),
            text: typed_text(event),
            location: code.map_or(KeyLocation::Standard, location),
            state: if pressed { ElementState::Pressed } else { ElementState::Released },
            repeat: event.kind == KeyKind::Repeat,
            platform_specific: KeyEventExtra {},
        };
        (key_event, modifiers)
    }

    /// No modifier held: what a window that lost the keyboard is left with, since the keys'
    /// releases go elsewhere.
    pub(super) fn release_all(&mut self) -> Option<Modifiers> {
        let held = std::mem::take(&mut self.held);
        (!held.is_empty()).then(|| modifiers(ModifiersKeys::empty()))
    }

    /// Takes `key` as held or let go, and answers the modifiers held now if that changed them.
    fn hold(&mut self, key: ModifiersKeys, pressed: bool) -> Option<Modifiers> {
        let before = self.held;
        self.held.set(key, pressed);
        (self.held != before).then(|| modifiers(self.held))
    }
}

fn modifiers(held: ModifiersKeys) -> Modifiers {
    use crate::keyboard::ModifiersState as State;
    let sides = [
        (ModifiersKeys::LSHIFT | ModifiersKeys::RSHIFT, State::SHIFT),
        (ModifiersKeys::LCONTROL | ModifiersKeys::RCONTROL, State::CONTROL),
        (ModifiersKeys::LALT | ModifiersKeys::RALT, State::ALT),
        (ModifiersKeys::LSUPER | ModifiersKeys::RSUPER, State::SUPER),
    ];
    let mut state = State::empty();
    for (keys, modifier) in sides {
        state.set(modifier, held.intersects(keys));
    }
    Modifiers { state, pressed_mods: held }
}

/// The modifier a key is, read from the key the layout makes it: a key WaOS sends to keep the
/// modifiers right, after a window was away, says which key it is and not where.
fn modifier_key(logical: u64) -> Option<ModifiersKeys> {
    Some(match logical {
        logical::SHIFT_LEFT => ModifiersKeys::LSHIFT,
        logical::SHIFT_RIGHT => ModifiersKeys::RSHIFT,
        logical::CONTROL_LEFT => ModifiersKeys::LCONTROL,
        logical::CONTROL_RIGHT => ModifiersKeys::RCONTROL,
        logical::ALT_LEFT => ModifiersKeys::LALT,
        logical::ALT_RIGHT => ModifiersKeys::RALT,
        logical::META_LEFT => ModifiersKeys::LSUPER,
        logical::META_RIGHT => ModifiersKeys::RSUPER,
        _ => return None,
    })
}

/// The key the layout makes of the event: one winit names, else the character it types, else
/// the character WaOS numbers it by.
fn logical_key(event: &ViewKeyEvent) -> Key {
    if let Some(named) = named_key(event.logical) {
        return Key::Named(named);
    }
    if let Some(character) = event.character.as_deref().filter(|text| is_printable(text)) {
        return Key::Character(SmolStr::new(character));
    }
    let numbered = (event.logical & logical::PLANE_MASK == logical::UNICODE_PLANE)
        .then(|| char::from_u32((event.logical & logical::VALUE_MASK) as u32))
        .flatten();
    match numbered {
        Some(character) => Key::Character(SmolStr::new(character.encode_utf8(&mut [0; 4]))),
        None => Key::Unidentified(NativeKey::Unidentified),
    }
}

/// What the key types as it goes down, none as it goes up.
fn typed_text(event: &ViewKeyEvent) -> Option<SmolStr> {
    let text = event.character.as_deref().filter(|text| !text.is_empty())?;
    (event.kind != KeyKind::Up).then(|| SmolStr::new(text))
}

fn is_printable(text: &str) -> bool {
    !text.is_empty() && !text.chars().any(char::is_control)
}

fn location(code: KeyCode) -> KeyLocation {
    match code {
        KeyCode::ShiftLeft | KeyCode::ControlLeft | KeyCode::AltLeft | KeyCode::SuperLeft => {
            KeyLocation::Left
        },
        KeyCode::ShiftRight | KeyCode::ControlRight | KeyCode::AltRight | KeyCode::SuperRight => {
            KeyLocation::Right
        },
        KeyCode::Numpad0
        | KeyCode::Numpad1
        | KeyCode::Numpad2
        | KeyCode::Numpad3
        | KeyCode::Numpad4
        | KeyCode::Numpad5
        | KeyCode::Numpad6
        | KeyCode::Numpad7
        | KeyCode::Numpad8
        | KeyCode::Numpad9
        | KeyCode::NumpadAdd
        | KeyCode::NumpadComma
        | KeyCode::NumpadDecimal
        | KeyCode::NumpadDivide
        | KeyCode::NumpadEnter
        | KeyCode::NumpadEqual
        | KeyCode::NumpadMultiply
        | KeyCode::NumpadSubtract => KeyLocation::Numpad,
        _ => KeyLocation::Standard,
    }
}
