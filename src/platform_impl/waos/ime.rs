//! winit's IME over a view's text input.
//!
//! The two models differ. A view's text input is Flutter's: the app tells WaOS the whole text and
//! selection of the field being edited, and an input method answers with the whole new value.
//! winit's app keeps its own text, and hears an input method's work as steps: text being composed
//! (`Ime::Preedit`), text committed (`Ime::Commit`). So WaOS is told an empty field, and each value
//! that comes back is the step itself: what is composed while it carries a composing range, what
//! is committed once it does not, after which the field is told empty again. WaOS's egui adapter
//! does the same.
//!
//! Plain typing is not the input method's: as winit's macOS backend does, a character typed with
//! nothing composed arrives as a key, with its text, and only composed text as `Ime::Commit`. A
//! key during a composition is the input method's, which WaOS hands it before the app.
//!
//! The invariant: what is reported committed is never reported again. Text outside the composing
//! range, which an input method can leave there (one syllable committed as the next is begun), is
//! committed once, and the field is told empty only once nothing is being composed, so a
//! composition is never cut short.

use std::sync::Mutex;

use wasdk::views::{
    PixelRect, TextAffinity, TextCapitalization, TextEditingValue, TextInput, TextInputAction,
    TextInputConfiguration, TextInputType, TextSelection, View,
};

use crate::event::Ime;
use crate::window::ImePurpose;

/// A window's text input, as winit's IME: shared by the window, which the app allows it through,
/// and the event loop, which reads the values that come back.
pub(super) struct ImeSession {
    input: Option<TextInput>,
    state: Mutex<SessionState>,
}

#[derive(Default)]
struct SessionState {
    allowed: bool,
    purpose: ImePurpose,
    /// Whether the app was told `Ime::Enabled` since it allowed the IME.
    enabled_told: bool,
    /// Whether the app shows text being composed.
    preedit_shown: bool,
    /// What of the field's text, outside what is composed, was reported committed.
    committed: String,
}

impl ImeSession {
    pub(super) fn of(view: &View) -> ImeSession {
        ImeSession { input: view.text_input(), state: Mutex::new(SessionState::default()) }
    }

    /// The app allows the IME or no longer does: WaOS's text input is taken for an empty field
    /// and shown, or given up and hidden.
    pub(super) fn allow(&self, allowed: bool) {
        let mut state = self.state.lock().unwrap();
        if state.allowed == allowed {
            return;
        }
        let Some(input) = &self.input else { return };
        if allowed {
            input.set_client(configuration(state.purpose));
            input.set_editing_state(&empty_value());
            input.show();
        } else {
            input.clear_client();
            input.hide();
        }
        *state = SessionState { allowed, purpose: state.purpose, ..SessionState::default() };
    }

    /// What the field is for, which WaOS's input methods may follow.
    pub(super) fn set_purpose(&self, purpose: ImePurpose) {
        let mut state = self.state.lock().unwrap();
        if state.purpose == purpose {
            return;
        }
        state.purpose = purpose;
        if let (true, Some(input)) = (state.allowed, &self.input) {
            input.update_config(configuration(purpose));
        }
    }

    /// Where the caret is, in the view's pixels, for the input method's candidate window.
    pub(super) fn set_cursor_area(&self, area: PixelRect) {
        if let (true, Some(input)) = (self.state.lock().unwrap().allowed, &self.input) {
            input.set_caret_rect(area);
            input.set_composing_rect(area);
        }
    }

    /// An input method's new value, as winit's IME events; none while the app does not allow
    /// the IME.
    pub(super) fn value_changed(&self, value: TextEditingValue) -> Vec<Ime> {
        let mut state = self.state.lock().unwrap();
        if !state.allowed {
            return Vec::new();
        }
        let step = Step::of(&value);
        let mut events = Vec::new();
        let new_commit =
            step.committed.strip_prefix(state.committed.as_str()).unwrap_or_else(|| {
                tracing::warn!(
                    "an input method changed text it had committed; only what follows is told"
                );
                &step.committed
            });
        if !state.enabled_told && (!new_commit.is_empty() || step.composed.is_some()) {
            state.enabled_told = true;
            events.push(Ime::Enabled);
        }
        let preedit_ends = !new_commit.is_empty() || step.composed.is_none();
        if preedit_ends && std::mem::take(&mut state.preedit_shown) {
            events.push(Ime::Preedit(String::new(), None));
        }
        if !new_commit.is_empty() {
            events.push(Ime::Commit(new_commit.to_owned()));
        }
        match step.composed {
            Some((text, cursor)) => {
                state.committed = step.committed;
                state.preedit_shown = true;
                events.push(Ime::Preedit(text, cursor));
            },
            None => {
                state.committed.clear();
                if !value.text.is_empty() {
                    if let Some(input) = &self.input {
                        input.set_editing_state(&empty_value());
                    }
                }
            },
        }
        events
    }

    /// WaOS ended the editing: the app hears the IME is disabled, if it heard it enabled.
    pub(super) fn closed(&self) -> Vec<Ime> {
        let mut state = self.state.lock().unwrap();
        let told = state.enabled_told;
        *state = SessionState { purpose: state.purpose, ..SessionState::default() };
        if told {
            vec![Ime::Disabled]
        } else {
            Vec::new()
        }
    }
}

/// What a value is, as steps: the text outside what is composed, which is committed, and what is
/// composed with the cursor within it.
struct Step {
    committed: String,
    composed: Option<(String, Option<(usize, usize)>)>,
}

impl Step {
    fn of(value: &TextEditingValue) -> Step {
        let Some(range) = &value.composing else {
            return Step { committed: value.text.clone(), composed: None };
        };
        let start = byte_offset(&value.text, range.start);
        let end = byte_offset(&value.text, range.end).max(start);
        let composed = value.text[start..end].to_owned();
        let committed = format!("{}{}", &value.text[..start], &value.text[end..]);
        let cursor = value.selection.as_ref().and_then(|selection| {
            let base = byte_offset(&value.text, selection.base_offset);
            let extent = byte_offset(&value.text, selection.extent_offset);
            let (from, to) = (base.min(extent), base.max(extent));
            (start <= from && to <= end).then(|| (from - start, to - start))
        });
        Step { committed, composed: Some((composed, cursor)) }
    }
}

/// The byte offset in `text` of `utf16_offset`, an offset in UTF-16 code units as WaOS counts
/// them; the end of the text past its end.
fn byte_offset(text: &str, utf16_offset: u32) -> usize {
    let mut units = 0;
    for (index, character) in text.char_indices() {
        if units >= utf16_offset as usize {
            return index;
        }
        units += character.len_utf16();
    }
    text.len()
}

fn configuration(purpose: ImePurpose) -> TextInputConfiguration {
    let password = purpose == ImePurpose::Password;
    TextInputConfiguration {
        input_type: if password { TextInputType::VisiblePassword } else { TextInputType::Text },
        input_action: TextInputAction::Done,
        obscure_text: password,
        autocorrect: purpose == ImePurpose::Normal,
        enable_suggestions: purpose == ImePurpose::Normal,
        text_capitalization: TextCapitalization::None,
        read_only: false,
    }
}

fn empty_value() -> TextEditingValue {
    TextEditingValue {
        text: String::new(),
        selection: Some(TextSelection {
            base_offset: 0,
            extent_offset: 0,
            affinity: TextAffinity::Downstream,
            is_directional: false,
        }),
        composing: None,
    }
}
