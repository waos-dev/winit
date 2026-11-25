//! A window: one of WaOS's views, with the window WaOS shows it in.
//!
//! WaOS opens a window when asked and sends its view later, where winit answers a window at
//! once, so opening one waits here for its view (`view_requested`). What draws on the window,
//! or names it a dialog's parent, reaches the view through the window's handle, which is the
//! view's own (`wasdk`'s, on `View`).
//!
//! The invariant: the view's events are taken once, as the window opens, and handed to the
//! event loop (`ActiveEventLoop::opened`), which is the only reader of them.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rwh_06::{HasDisplayHandle, HasWindowHandle};
use wasdk::system::{self, Brightness};
use wasdk::views::{self, Cursor as ViewCursor, View, ViewChange};
use wasdk::windows::{self, WindowBackground, WindowConfig};

use super::event_loop::OpenedWindow;
use super::{ActiveEventLoop, MonitorHandle, OsError, WindowId};
use crate::cursor::Cursor;
use crate::dpi::{LogicalSize, PhysicalPosition, PhysicalSize, Position, Size};
use crate::error;
use crate::platform_impl::Fullscreen;
use crate::window::{
    CursorGrabMode, CursorIcon, ImePurpose, ResizeDirection, Theme, UserAttentionType,
    WindowAttributes, WindowButtons, WindowLevel,
};

pub struct Window {
    view: Arc<View>,
    title: Mutex<String>,
    cursor: Mutex<ShownCursor>,
    focused: Arc<AtomicBool>,
    decorated: bool,
    resizable: AtomicBool,
}

/// The cursor the app chose, and whether it is shown.
struct ShownCursor {
    icon: ViewCursor,
    visible: bool,
}

impl Window {
    pub(crate) fn new(
        event_loop: &ActiveEventLoop,
        attributes: WindowAttributes,
    ) -> Result<Self, error::OsError> {
        let config = window_config(&attributes);
        let view = view_requested(&config).map_err(|refusal| os_error!(OsError(refusal)))?;
        let window = Window {
            view: Arc::new(view),
            title: Mutex::new(attributes.title.clone()),
            cursor: Mutex::new(ShownCursor { icon: ViewCursor::Default, visible: true }),
            focused: Arc::new(AtomicBool::new(false)),
            decorated: attributes.decorations,
            resizable: AtomicBool::new(attributes.resizable),
        };
        window.set_min_inner_size(attributes.min_inner_size);
        window.set_max_inner_size(attributes.max_inner_size);
        if attributes.maximized {
            window.set_maximized(true);
        }
        if let Some(events) = window.view.events() {
            event_loop.opened(OpenedWindow {
                id: window.id(),
                view: window.view.clone(),
                events,
                focused: Arc::clone(&window.focused),
            });
        }
        Ok(window)
    }

    pub(crate) fn maybe_queue_on_main(&self, f: impl FnOnce(&Self) + Send + 'static) {
        f(self)
    }

    pub(crate) fn maybe_wait_on_main<R: Send>(&self, f: impl FnOnce(&Self) -> R + Send) -> R {
        f(self)
    }

    /// The window WaOS shows the view in; none where the view fills a screen of its own, as
    /// on a phone.
    fn window(&self) -> Option<windows::Window> {
        self.view.window()
    }

    #[inline]
    pub fn id(&self) -> WindowId {
        WindowId(self.view.id())
    }

    #[inline]
    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        MonitorHandle::all().into_iter().next()
    }

    #[inline]
    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        MonitorHandle::all().into()
    }

    #[inline]
    pub fn current_monitor(&self) -> Option<MonitorHandle> {
        self.view.screen().as_ref().map(MonitorHandle::of)
    }

    #[inline]
    pub fn scale_factor(&self) -> f64 {
        self.view.metrics().device_pixel_ratio
    }

    #[inline]
    pub fn request_redraw(&self) {
        self.view.request_frame();
    }

    #[inline]
    pub fn pre_present_notify(&self) {}

    #[inline]
    pub fn reset_dead_keys(&self) {}

    #[inline]
    pub fn inner_position(&self) -> Result<PhysicalPosition<i32>, error::NotSupportedError> {
        self.outer_position()
    }

    #[inline]
    pub fn outer_position(&self) -> Result<PhysicalPosition<i32>, error::NotSupportedError> {
        let window = self.window().ok_or_else(error::NotSupportedError::new)?;
        let (origin, scale) = (window.frame().origin, self.scale_factor());
        Ok(PhysicalPosition::new((origin.x * scale) as i32, (origin.y * scale) as i32))
    }

    #[inline]
    pub fn set_outer_position(&self, position: Position) {
        let Some(window) = self.window() else { return };
        let origin = position.to_logical::<f64>(self.scale_factor());
        let mut frame = window.frame();
        frame.origin = windows::Point { x: origin.x, y: origin.y };
        window.set_frame(frame, None);
    }

    #[inline]
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        let metrics = self.view.metrics();
        PhysicalSize::new(metrics.width, metrics.height)
    }

    /// Asks WaOS for the size, which arrives as a `Resized` once the window has it.
    #[inline]
    pub fn request_inner_size(&self, size: Size) -> Option<PhysicalSize<u32>> {
        let window = self.window()?;
        let scale = self.scale_factor();
        let (wanted, now): (LogicalSize<f64>, LogicalSize<f64>) =
            (size.to_logical(scale), self.inner_size().to_logical(scale));
        // The frame is the window's, title bar and all: it grows by what the view should.
        let mut frame = window.frame();
        frame.size.width += wanted.width - now.width;
        frame.size.height += wanted.height - now.height;
        window.set_frame(frame, None);
        None
    }

    #[inline]
    pub fn outer_size(&self) -> PhysicalSize<u32> {
        let Some(window) = self.window() else { return self.inner_size() };
        let (size, scale) = (window.frame().size, self.scale_factor());
        PhysicalSize::new((size.width * scale) as u32, (size.height * scale) as u32)
    }

    #[inline]
    pub fn set_min_inner_size(&self, size: Option<Size>) {
        if let Some(limits) = self.window().and_then(|window| window.size_limits()) {
            limits.set_min_size(size.map(|size| self.window_size(size)));
        }
    }

    #[inline]
    pub fn set_max_inner_size(&self, size: Option<Size>) {
        if let Some(limits) = self.window().and_then(|window| window.size_limits()) {
            limits.set_max_size(size.map(|size| self.window_size(size)));
        }
    }

    /// `size` in the units WaOS sizes a window in.
    fn window_size(&self, size: Size) -> windows::Size {
        let size = size.to_logical::<f64>(self.scale_factor());
        windows::Size { width: size.width, height: size.height }
    }

    #[inline]
    pub fn title(&self) -> String {
        self.title.lock().unwrap().clone()
    }

    #[inline]
    pub fn set_title(&self, title: &str) {
        title.clone_into(&mut self.title.lock().unwrap());
        if let Some(window) = self.window() {
            window.set_title(title);
        }
    }

    #[inline]
    pub fn set_transparent(&self, _transparent: bool) {}

    #[inline]
    pub fn set_blur(&self, _blur: bool) {}

    #[inline]
    pub fn set_visible(&self, visible: bool) {
        match (self.window(), visible) {
            (Some(window), true) => window.show(),
            (Some(window), false) => window.hide(),
            (None, _) => {},
        }
    }

    #[inline]
    pub fn is_visible(&self) -> Option<bool> {
        None
    }

    #[inline]
    pub fn resize_increments(&self) -> Option<PhysicalSize<u32>> {
        None
    }

    #[inline]
    pub fn set_resize_increments(&self, _increments: Option<Size>) {}

    #[inline]
    pub fn set_resizable(&self, _resizable: bool) {}

    #[inline]
    pub fn is_resizable(&self) -> bool {
        self.resizable.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_minimized(&self, minimized: bool) {
        if let Some(window) = self.window() {
            window.set_minimized(minimized);
        }
    }

    #[inline]
    pub fn is_minimized(&self) -> Option<bool> {
        None
    }

    #[inline]
    pub fn set_maximized(&self, maximized: bool) {
        if let Some(window) = self.window() {
            window.set_maximized(maximized);
        }
    }

    #[inline]
    pub fn is_maximized(&self) -> bool {
        self.window().is_some_and(|window| window.is_maximized())
    }

    /// The system's own full screen for a window, on the monitor named or the window's own;
    /// WaOS shows a screen in one mode, so an exclusive mode is that too. Nothing for a view in
    /// no window, which fills its screen as it is. The window is full screen once WaOS says so,
    /// which a `Resized` follows.
    #[inline]
    pub(crate) fn set_fullscreen(&self, fullscreen: Option<Fullscreen>) {
        let Some(window) = self.window() else { return };
        let screen = match &fullscreen {
            Some(Fullscreen::Borderless(Some(monitor))) => Some(monitor.id),
            Some(Fullscreen::Exclusive(mode)) => Some(mode.monitor.id),
            Some(Fullscreen::Borderless(None)) | None => None,
        };
        window.set_fullscreen(fullscreen.is_some(), screen);
    }

    #[inline]
    pub(crate) fn fullscreen(&self) -> Option<Fullscreen> {
        let fullscreen = self.window().is_some_and(|window| window.is_fullscreen());
        fullscreen.then(|| Fullscreen::Borderless(self.current_monitor()))
    }

    #[inline]
    pub fn set_decorations(&self, _decorations: bool) {}

    #[inline]
    pub fn is_decorated(&self) -> bool {
        self.decorated
    }

    #[inline]
    pub fn set_window_level(&self, _level: WindowLevel) {}

    #[inline]
    pub fn set_window_icon(&self, _window_icon: Option<crate::icon::Icon>) {}

    #[inline]
    pub fn set_ime_cursor_area(&self, _position: Position, _size: Size) {}

    #[inline]
    pub fn set_ime_allowed(&self, _allowed: bool) {}

    #[inline]
    pub fn set_ime_purpose(&self, _purpose: ImePurpose) {}

    #[inline]
    pub fn focus_window(&self) {}

    #[inline]
    pub fn request_user_attention(&self, _request_type: Option<UserAttentionType>) {}

    #[inline]
    pub fn set_cursor(&self, cursor: Cursor) {
        let Cursor::Icon(icon) = cursor else { return };
        let mut shown = self.cursor.lock().unwrap();
        shown.icon = view_cursor(icon);
        if shown.visible {
            self.view.set_cursor(shown.icon);
        }
    }

    #[inline]
    pub fn set_cursor_position(&self, _: Position) -> Result<(), error::ExternalError> {
        Err(error::ExternalError::NotSupported(error::NotSupportedError::new()))
    }

    #[inline]
    pub fn set_cursor_grab(&self, mode: CursorGrabMode) -> Result<(), error::ExternalError> {
        match mode {
            CursorGrabMode::None => Ok(()),
            CursorGrabMode::Confined | CursorGrabMode::Locked => {
                Err(error::ExternalError::NotSupported(error::NotSupportedError::new()))
            },
        }
    }

    #[inline]
    pub fn set_cursor_visible(&self, visible: bool) {
        let mut shown = self.cursor.lock().unwrap();
        shown.visible = visible;
        self.view.set_cursor(if visible { shown.icon } else { ViewCursor::None });
    }

    #[inline]
    pub fn drag_window(&self) -> Result<(), error::ExternalError> {
        let window = self
            .window()
            .ok_or_else(|| error::ExternalError::NotSupported(error::NotSupportedError::new()))?;
        window.drag();
        Ok(())
    }

    #[inline]
    pub fn drag_resize_window(
        &self,
        _direction: ResizeDirection,
    ) -> Result<(), error::ExternalError> {
        Err(error::ExternalError::NotSupported(error::NotSupportedError::new()))
    }

    #[inline]
    pub fn show_window_menu(&self, _position: Position) {}

    #[inline]
    pub fn set_cursor_hittest(&self, _hittest: bool) -> Result<(), error::ExternalError> {
        Err(error::ExternalError::NotSupported(error::NotSupportedError::new()))
    }

    /// The view's own handle, which a renderer makes its surface from and a dialog finds its
    /// parent by.
    #[inline]
    pub fn raw_window_handle_rwh_06(&self) -> Result<rwh_06::RawWindowHandle, rwh_06::HandleError> {
        self.view.window_handle().map(|handle| handle.as_raw())
    }

    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        self.view.display_handle().map(|handle| handle.as_raw())
    }

    #[inline]
    pub fn set_enabled_buttons(&self, _buttons: WindowButtons) {}

    #[inline]
    pub fn enabled_buttons(&self) -> WindowButtons {
        WindowButtons::all()
    }

    /// The user's light or dark, which WaOS's windows follow.
    #[inline]
    pub fn theme(&self) -> Option<Theme> {
        Some(theme_of(system::platform_brightness()))
    }

    #[inline]
    pub fn has_focus(&self) -> bool {
        self.focused.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_theme(&self, _theme: Option<Theme>) {}

    #[inline]
    pub fn set_content_protected(&self, _protected: bool) {}
}

impl Drop for Window {
    /// Closes the window, which ends its view's events: the event loop reads that as the
    /// window destroyed.
    fn drop(&mut self) {
        if let Some(window) = self.window() {
            window.close();
        }
    }
}

pub(super) fn theme_of(brightness: Brightness) -> Theme {
    match brightness {
        Brightness::Light => Theme::Light,
        Brightness::Dark => Theme::Dark,
    }
}

/// What WaOS is asked for a window by: `attributes` as far as it has them. Sizes are in
/// WaOS's own units, which a physical size is read as at a scale of 1, the window's screen
/// not being known yet.
fn window_config(attributes: &WindowAttributes) -> WindowConfig {
    let config = WindowConfig::new();
    config.set_title(&attributes.title);
    if let Some(size) = attributes.inner_size {
        let size = size.to_logical::<f64>(1.0);
        config.set_size(windows::Size { width: size.width, height: size.height });
    }
    if let Some(position) = attributes.position {
        let position = position.to_logical::<f64>(1.0);
        config.set_position(windows::Point { x: position.x, y: position.y });
    }
    config.set_decorations(attributes.decorations);
    config.set_resizable(attributes.resizable);
    config.set_visible(attributes.visible);
    config.set_level(match attributes.window_level {
        WindowLevel::AlwaysOnBottom => windows::WindowLevel::AlwaysOnBottom,
        WindowLevel::Normal => windows::WindowLevel::Normal,
        WindowLevel::AlwaysOnTop => windows::WindowLevel::AlwaysOnTop,
    });
    if attributes.transparent {
        config.set_background(WindowBackground::Transparent);
    }
    config
}

/// Asks WaOS for a window made as `config` says and waits, on the app's thread, until its
/// view is there or WaOS refuses it.
fn view_requested(config: &WindowConfig) -> Result<View, String> {
    // The changes are listened to before the request, so the view that answers it is not
    // missed.
    let mut changes = views::view_changes();
    let request = views::request_view(config, None).map_err(|error| error.to_string())?;
    wasdk::block_on(async {
        while let Some(change) = changes.next().await {
            match change {
                ViewChange::Added(view) if view.request_id() == request => return Ok(view),
                ViewChange::RequestFailed(failure) if failure.request == request => {
                    return Err(failure.error.to_string());
                },
                _ => {},
            }
        }
        Err("WaOS ended the app's views before the window's arrived".to_owned())
    })
}

fn view_cursor(icon: CursorIcon) -> ViewCursor {
    match icon {
        CursorIcon::Default => ViewCursor::Default,
        CursorIcon::ContextMenu => ViewCursor::ContextMenu,
        CursorIcon::Help => ViewCursor::Help,
        CursorIcon::Pointer => ViewCursor::Pointer,
        CursorIcon::Progress => ViewCursor::Progress,
        CursorIcon::Wait => ViewCursor::Wait,
        CursorIcon::Cell => ViewCursor::Cell,
        CursorIcon::Crosshair => ViewCursor::Crosshair,
        CursorIcon::Text => ViewCursor::Text,
        CursorIcon::VerticalText => ViewCursor::VerticalText,
        CursorIcon::Alias => ViewCursor::Alias,
        CursorIcon::Copy => ViewCursor::Copy,
        CursorIcon::Move => ViewCursor::Move,
        CursorIcon::NoDrop => ViewCursor::NoDrop,
        CursorIcon::NotAllowed => ViewCursor::NotAllowed,
        CursorIcon::Grab => ViewCursor::Grab,
        CursorIcon::Grabbing => ViewCursor::Grabbing,
        CursorIcon::EResize => ViewCursor::EResize,
        CursorIcon::NResize => ViewCursor::NResize,
        CursorIcon::NeResize => ViewCursor::NeResize,
        CursorIcon::NwResize => ViewCursor::NwResize,
        CursorIcon::SResize => ViewCursor::SResize,
        CursorIcon::SeResize => ViewCursor::SeResize,
        CursorIcon::SwResize => ViewCursor::SwResize,
        CursorIcon::WResize => ViewCursor::WResize,
        CursorIcon::EwResize => ViewCursor::EwResize,
        CursorIcon::NsResize => ViewCursor::NsResize,
        CursorIcon::NeswResize => ViewCursor::NeswResize,
        CursorIcon::NwseResize => ViewCursor::NwseResize,
        CursorIcon::ColResize => ViewCursor::ColResize,
        CursorIcon::RowResize => ViewCursor::RowResize,
        CursorIcon::AllScroll => ViewCursor::AllScroll,
        CursorIcon::ZoomIn => ViewCursor::ZoomIn,
        CursorIcon::ZoomOut => ViewCursor::ZoomOut,
        _ => ViewCursor::Default,
    }
}
