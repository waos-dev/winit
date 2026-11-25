//! WaOS: a winit app as a WaOS app, a WASI component built for `wasm32-wasip3`.
//!
//! A window is one of WaOS's views with the window it is shown in (`window`). The event loop
//! runs on the app's one thread until the app ends, reading each view's events as they arrive
//! (`event_loop`) and handing them on as winit's (`pointer`, `keyboard`).
//!
//! What WaOS has and winit 0.30 has no place for is left out: the Edit menu's and the
//! clipboard's commands a view is sent, and a key's answer whether the app used it, which is
//! always that it did.
//!
//! Only `raw-window-handle` 0.6 is served (`rwh_06`): a view's handle has no older form.

#![cfg(target_os = "wasi")]

use std::fmt::{self, Display, Formatter};

use crate::dpi::{PhysicalPosition, PhysicalSize};

pub(crate) use self::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy, OwnedDisplayHandle};
mod event_loop;

pub use self::window::Window;
mod window;

mod key_names;
mod keyboard;
mod pointer;

pub(crate) use crate::cursor::{
    NoCustomCursor as PlatformCustomCursor, NoCustomCursor as PlatformCustomCursorSource,
};
pub(crate) use crate::icon::NoIcon as PlatformIcon;

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PlatformSpecificEventLoopAttributes {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlatformSpecificWindowAttributes;

/// A window's id: its view's, which WaOS gives no two views of an app.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WindowId(u64);

impl WindowId {
    pub const fn dummy() -> Self {
        WindowId(u64::MAX)
    }
}

impl From<WindowId> for u64 {
    fn from(id: WindowId) -> Self {
        id.0
    }
}

impl From<u64> for WindowId {
    fn from(id: u64) -> Self {
        WindowId(id)
    }
}

/// The device an event came from: a pointer's number as WaOS gives it, and 0 for a keyboard.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId(i64);

impl DeviceId {
    pub const fn dummy() -> Self {
        DeviceId(i64::MAX)
    }
}

/// Why WaOS refused something, in its own words.
#[derive(Clone, Debug)]
pub struct OsError(String);

impl Display for OsError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One of the screens WaOS shows windows on, as it was when it was asked for.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MonitorHandle {
    id: u64,
    name: String,
    size: (u32, u32),
    position: (i32, i32),
    /// `f64::to_bits` of the scale, so that the handle compares and hashes.
    scale_bits: u64,
    refresh_rate_millihertz: u32,
}

impl MonitorHandle {
    fn of(screen: &wasdk::screens::Screen) -> MonitorHandle {
        let (size, scale, frame) = (screen.size(), screen.scale(), screen.frame());
        MonitorHandle {
            id: screen.id(),
            name: screen.name(),
            size: (size.width, size.height),
            position: ((frame.origin.x * scale) as i32, (frame.origin.y * scale) as i32),
            scale_bits: scale.to_bits(),
            refresh_rate_millihertz: screen.refresh_rate().saturating_mul(1000),
        }
    }

    /// The screens WaOS shows windows on now.
    fn all() -> Vec<MonitorHandle> {
        wasdk::screens::screens().iter().map(MonitorHandle::of).collect()
    }

    pub fn name(&self) -> Option<String> {
        Some(self.name.clone())
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size.into()
    }

    pub fn position(&self) -> PhysicalPosition<i32> {
        self.position.into()
    }

    pub fn scale_factor(&self) -> f64 {
        f64::from_bits(self.scale_bits)
    }

    pub fn refresh_rate_millihertz(&self) -> Option<u32> {
        (self.refresh_rate_millihertz > 0).then_some(self.refresh_rate_millihertz)
    }

    /// The one mode WaOS shows a screen in: its own.
    pub fn video_modes(&self) -> impl Iterator<Item = VideoModeHandle> {
        std::iter::once(VideoModeHandle { monitor: self.clone() })
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VideoModeHandle {
    monitor: MonitorHandle,
}

impl VideoModeHandle {
    pub fn size(&self) -> PhysicalSize<u32> {
        self.monitor.size()
    }

    pub fn bit_depth(&self) -> u16 {
        32
    }

    pub fn refresh_rate_millihertz(&self) -> u32 {
        self.monitor.refresh_rate_millihertz
    }

    pub fn monitor(&self) -> MonitorHandle {
        self.monitor.clone()
    }
}

/// Nothing: WaOS says no more of a key than winit's own `KeyEvent` holds.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEventExtra {}
