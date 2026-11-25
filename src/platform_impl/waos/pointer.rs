//! A view's pointer events as winit's.
//!
//! WaOS sends Flutter's `PointerData`: one record for a mouse, a finger and a trackpad's
//! gesture, with the buttons held as bits and a gesture's pan, scale and rotation as totals
//! since it began. winit names a button as it goes down or up and a gesture by its steps, so
//! the buttons last held and the gesture's last totals are kept here.

use wasdk::views::{
    PointerButtons, PointerChange, PointerDeviceKind, PointerEvent, PointerSignalKind,
};

use super::DeviceId;
use crate::dpi::PhysicalPosition;
use crate::event::{
    DeviceId as RootDeviceId, ElementState, Force, MouseButton, MouseScrollDelta, Touch,
    TouchPhase, WindowEvent,
};

const BUTTONS: [(PointerButtons, MouseButton); 5] = [
    (PointerButtons::PRIMARY, MouseButton::Left),
    (PointerButtons::SECONDARY, MouseButton::Right),
    (PointerButtons::TERTIARY, MouseButton::Middle),
    (PointerButtons::BACK, MouseButton::Back),
    (PointerButtons::FORWARD, MouseButton::Forward),
];

/// The pointers over one window.
pub(super) struct Pointers {
    buttons: PointerButtons,
    inside: bool,
    gesture: Gesture,
}

/// A trackpad gesture's totals since it began.
#[derive(Clone, Copy)]
struct Gesture {
    pan: (f64, f64),
    scale: f64,
    rotation: f64,
}

impl Gesture {
    const START: Gesture = Gesture { pan: (0.0, 0.0), scale: 1.0, rotation: 0.0 };
}

impl Pointers {
    pub(super) fn new() -> Pointers {
        Pointers { buttons: PointerButtons::empty(), inside: false, gesture: Gesture::START }
    }

    /// `event` as winit's events, in the order winit sends them.
    pub(super) fn translate(&mut self, event: &PointerEvent) -> Vec<WindowEvent> {
        let device_id = RootDeviceId(DeviceId(event.device));
        match event.kind {
            PointerDeviceKind::Touch => touch(event, device_id).into_iter().collect(),
            _ => self.cursor(event, device_id),
        }
    }

    fn cursor(&mut self, event: &PointerEvent, device_id: RootDeviceId) -> Vec<WindowEvent> {
        match event.change {
            PointerChange::Remove => self.left(device_id),
            PointerChange::Cancel => {
                let mut events = self.button_changes(PointerButtons::empty(), device_id);
                events.extend(self.left(device_id));
                events
            },
            PointerChange::PanZoomStart => {
                self.gesture = Gesture::START;
                vec![pan(device_id, (0.0, 0.0), TouchPhase::Started)]
            },
            PointerChange::PanZoomUpdate => self.gesture_step(event, device_id),
            PointerChange::PanZoomEnd => vec![pan(device_id, (0.0, 0.0), TouchPhase::Ended)],
            PointerChange::Add
            | PointerChange::Hover
            | PointerChange::Down
            | PointerChange::Move
            | PointerChange::Up => {
                let mut events = Vec::new();
                if !std::mem::replace(&mut self.inside, true) {
                    events.push(WindowEvent::CursorEntered { device_id });
                }
                let position = PhysicalPosition::new(event.x, event.y);
                events.push(WindowEvent::CursorMoved { device_id, position });
                events.extend(self.button_changes(event.buttons, device_id));
                events.extend(signal(event, device_id));
                events
            },
        }
    }

    fn left(&mut self, device_id: RootDeviceId) -> Vec<WindowEvent> {
        match std::mem::replace(&mut self.inside, false) {
            true => vec![WindowEvent::CursorLeft { device_id }],
            false => Vec::new(),
        }
    }

    fn button_changes(
        &mut self,
        held: PointerButtons,
        device_id: RootDeviceId,
    ) -> Vec<WindowEvent> {
        let before = std::mem::replace(&mut self.buttons, held);
        BUTTONS
            .into_iter()
            .filter(|(bit, _)| before.contains(*bit) != held.contains(*bit))
            .map(|(bit, button)| WindowEvent::MouseInput {
                device_id,
                state: match held.contains(bit) {
                    true => ElementState::Pressed,
                    false => ElementState::Released,
                },
                button,
            })
            .collect()
    }

    fn gesture_step(&mut self, event: &PointerEvent, device_id: RootDeviceId) -> Vec<WindowEvent> {
        let now = Gesture {
            pan: (event.pan_x, event.pan_y),
            scale: event.scale,
            rotation: event.rotation,
        };
        let before = std::mem::replace(&mut self.gesture, now);
        let mut events = Vec::new();
        if now.pan != before.pan {
            let step = (now.pan.0 - before.pan.0, now.pan.1 - before.pan.1);
            events.push(pan(device_id, step, TouchPhase::Moved));
        }
        if now.scale != before.scale && before.scale > 0.0 && now.scale > 0.0 {
            events.push(WindowEvent::PinchGesture {
                device_id,
                // winit's step is what macOS calls a magnification: about the logarithm of the
                // ratio, as egui-winit and others read it.
                delta: (now.scale / before.scale).ln(),
                phase: TouchPhase::Moved,
            });
        }
        if now.rotation != before.rotation {
            events.push(WindowEvent::RotationGesture {
                device_id,
                // Flutter turns clockwise in radians, winit counterclockwise in degrees.
                delta: -(now.rotation - before.rotation).to_degrees() as f32,
                phase: TouchPhase::Moved,
            });
        }
        events
    }
}

/// A trackpad's two-finger pan, which winit sends as the wheel's scroll with a phase.
fn pan(device_id: RootDeviceId, step: (f64, f64), phase: TouchPhase) -> WindowEvent {
    let delta = MouseScrollDelta::PixelDelta(PhysicalPosition::new(step.0, step.1));
    WindowEvent::MouseWheel { device_id, delta, phase }
}

fn touch(event: &PointerEvent, device_id: RootDeviceId) -> Option<WindowEvent> {
    let phase = match event.change {
        PointerChange::Down => TouchPhase::Started,
        PointerChange::Move => TouchPhase::Moved,
        PointerChange::Up => TouchPhase::Ended,
        PointerChange::Cancel => TouchPhase::Cancelled,
        _ => return None,
    };
    let pressure_range = event.pressure_max - event.pressure_min;
    Some(WindowEvent::Touch(Touch {
        device_id,
        phase,
        location: PhysicalPosition::new(event.x, event.y),
        force: (pressure_range > 0.0)
            .then_some(Force::Normalized((event.pressure - event.pressure_min) / pressure_range)),
        id: event.device as u64,
    }))
}

fn signal(event: &PointerEvent, device_id: RootDeviceId) -> Option<WindowEvent> {
    match event.signal {
        PointerSignalKind::Scroll => {
            // Flutter's scroll is how far the view's position moves; winit's is how far the
            // content does, the other way.
            let step = (-event.scroll_delta_x, -event.scroll_delta_y);
            Some(pan(device_id, step, TouchPhase::Moved))
        },
        PointerSignalKind::Scale if event.scale > 0.0 => Some(WindowEvent::PinchGesture {
            device_id,
            delta: event.scale.ln(),
            phase: TouchPhase::Moved,
        }),
        PointerSignalKind::Scale
        | PointerSignalKind::None
        | PointerSignalKind::ScrollInertiaCancel => None,
    }
}
