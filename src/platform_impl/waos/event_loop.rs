//! The event loop: on the app's one thread until the app ends, waiting for what WaOS sends
//! next and handing it to the app as winit's events.
//!
//! WaOS sends an app its news as streams: one of events for each view, and one of the
//! system's changes. Other parts of the app wake the loop through its proxy. The loop waits
//! for whichever has something first (`Arrivals`), or until the time the app asked to be woken
//! at.
//!
//! The invariant: a window's events are read here and nowhere else, from the stream its
//! window handed over as it opened (`ActiveEventLoop::opened`); once that stream ends, the
//! window is destroyed and nothing more is said of it.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::{poll_fn, Future};
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Instant;

use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::stream::{Stream, StreamExt};
use wasdk::system::{self, SystemChange};
use wasdk::views::{View, ViewEvent, ViewFocusState};
use wasdk::StreamReader;

use super::keyboard::Keyboard;
use super::pointer::Pointers;
use super::window::theme_of;
use super::{DeviceId, MonitorHandle, PlatformSpecificEventLoopAttributes, WindowId};
use crate::dpi::PhysicalSize;
use crate::error::EventLoopError;
use crate::event::{DeviceId as RootDeviceId, Event, InnerSizeWriter, StartCause, WindowEvent};
use crate::event_loop::{self, ControlFlow, DeviceEvents};
use crate::window::{
    CustomCursor as RootCustomCursor, CustomCursorSource, Theme, WindowId as RootWindowId,
};

/// A window as it opened, for the event loop to read from then on.
pub(super) struct OpenedWindow {
    pub(super) id: WindowId,
    pub(super) view: Arc<View>,
    pub(super) events: StreamReader<ViewEvent>,
    pub(super) focused: Arc<AtomicBool>,
}

/// What the loop keeps of an open window, to make winit's events of its view's.
struct WindowEvents {
    id: WindowId,
    events: Pin<Box<dyn Stream<Item = ViewEvent>>>,
    pointers: Pointers,
    keyboard: Keyboard,
    focused: Arc<AtomicBool>,
    /// The size and scale the app was last told of.
    size: PhysicalSize<u32>,
    scale_factor: f64,
}

impl WindowEvents {
    fn of(opened: OpenedWindow) -> WindowEvents {
        let metrics = opened.view.metrics();
        WindowEvents {
            id: opened.id,
            events: Box::pin(opened.events.into_stream()),
            pointers: Pointers::new(),
            keyboard: Keyboard::default(),
            focused: opened.focused,
            size: PhysicalSize::new(metrics.width, metrics.height),
            scale_factor: metrics.device_pixel_ratio,
        }
    }

    /// One of the view's events as winit's, in the order winit sends them.
    fn translate(&mut self, event: ViewEvent) -> Vec<WindowEvent> {
        match event {
            ViewEvent::Frame(_) => vec![WindowEvent::RedrawRequested],
            ViewEvent::Metrics(metrics) => {
                let size = PhysicalSize::new(metrics.width, metrics.height);
                self.resized(size, metrics.device_pixel_ratio)
            },
            ViewEvent::Pointer(pointer) => self.pointers.translate(&pointer),
            ViewEvent::Key(input) => {
                let (event, modifiers) = self.keyboard.translate(&input.event);
                // winit has no way for the app to say it left the key alone.
                input.reply.handled(true);
                let device_id = RootDeviceId(DeviceId(0));
                let is_synthetic = input.event.synthesized;
                let mut events =
                    vec![WindowEvent::KeyboardInput { device_id, event, is_synthetic }];
                events.extend(modifiers.map(WindowEvent::ModifiersChanged));
                events
            },
            ViewEvent::Focus(focus) => {
                let focused = focus.state == ViewFocusState::Focused;
                self.focused.store(focused, Ordering::Relaxed);
                let mut events = vec![WindowEvent::Focused(focused)];
                if !focused {
                    events.extend(self.keyboard.release_all().map(WindowEvent::ModifiersChanged));
                }
                events
            },
            ViewEvent::CloseRequested => vec![WindowEvent::CloseRequested],
            // winit 0.30 has no event for these: the system's Back, text typed through the
            // system's text input, and the clipboard's and the Edit menu's commands.
            ViewEvent::PopRoute
            | ViewEvent::TextInputEditingValue(_)
            | ViewEvent::TextInputAction(_)
            | ViewEvent::TextInputClosed
            | ViewEvent::Copy(_)
            | ViewEvent::Cut(_)
            | ViewEvent::Paste(_)
            | ViewEvent::Edit(_) => Vec::new(),
        }
    }

    /// The view's size and scale as they are now, told where they changed.
    fn resized(&mut self, size: PhysicalSize<u32>, scale_factor: f64) -> Vec<WindowEvent> {
        let mut events = Vec::new();
        if scale_factor != self.scale_factor {
            self.scale_factor = scale_factor;
            // WaOS has set the view's size already, so what the app writes back is not read.
            let unread = Arc::new(Mutex::new(size));
            events.push(WindowEvent::ScaleFactorChanged {
                scale_factor,
                inner_size_writer: InnerSizeWriter::new(Arc::downgrade(&unread)),
            });
        }
        if size != self.size {
            self.size = size;
            events.push(WindowEvent::Resized(size));
        }
        events
    }
}

/// What the loop was woken by.
enum Arrival<T> {
    /// An event of a window's view, or none: the view's events ended.
    View(WindowId, Option<ViewEvent>),
    System(SystemChange),
    User(T),
    /// The time the app asked to be woken at came.
    ResumeTime,
}

/// What the loop waits on: every open window's events, the system's changes, and what the
/// app's proxies sent.
struct Arrivals<T> {
    windows: Vec<WindowEvents>,
    system: Pin<Box<dyn Stream<Item = SystemChange>>>,
    user: UnboundedReceiver<T>,
}

impl<T> Arrivals<T> {
    /// What arrived, if anything has: the windows' events before the system's changes, and
    /// the app's own last.
    fn poll_next(&mut self, context: &mut Context<'_>) -> Poll<Arrival<T>> {
        for window in &mut self.windows {
            if let Poll::Ready(event) = window.events.poll_next_unpin(context) {
                return Poll::Ready(Arrival::View(window.id, event));
            }
        }
        if let Poll::Ready(Some(change)) = self.system.poll_next_unpin(context) {
            return Poll::Ready(Arrival::System(change));
        }
        if let Poll::Ready(Some(event)) = self.user.poll_next_unpin(context) {
            return Poll::Ready(Arrival::User(event));
        }
        Poll::Pending
    }

    /// The next arrival, or the resume time where `resume_time` comes first.
    async fn next(&mut self, resume_time: Option<Instant>) -> Arrival<T> {
        let mut timer = resume_time.map(|time| Box::pin(wait_until(time)));
        poll_fn(|context| {
            if let Poll::Ready(arrival) = self.poll_next(context) {
                return Poll::Ready(arrival);
            }
            match timer.as_mut().map(|timer| timer.as_mut().poll(context)) {
                Some(Poll::Ready(())) => Poll::Ready(Arrival::ResumeTime),
                _ => Poll::Pending,
            }
        })
        .await
    }

    /// What has arrived already, if anything has, once WaOS had a turn to send it.
    async fn ready(&mut self) -> Option<Arrival<T>> {
        wasdk::yield_async().await;
        poll_fn(|context| match self.poll_next(context) {
            Poll::Ready(arrival) => Poll::Ready(Some(arrival)),
            Poll::Pending => Poll::Ready(None),
        })
        .await
    }
}

async fn wait_until(time: Instant) {
    let wait = time.saturating_duration_since(Instant::now());
    wasdk::monotonic_clock::wait_for(wait.as_nanos() as u64).await;
}

pub struct EventLoop<T: 'static> {
    arrivals: Arrivals<T>,
    window_target: event_loop::ActiveEventLoop,
    user_events: UnboundedSender<T>,
}

impl<T: 'static> EventLoop<T> {
    pub(crate) fn new(_: &PlatformSpecificEventLoopAttributes) -> Result<Self, EventLoopError> {
        let (user_events, user) = mpsc::unbounded();
        Ok(EventLoop {
            arrivals: Arrivals {
                windows: Vec::new(),
                system: Box::pin(system::changes().into_stream()),
                user,
            },
            window_target: event_loop::ActiveEventLoop {
                p: ActiveEventLoop {
                    control_flow: Cell::new(ControlFlow::default()),
                    exit: Cell::new(false),
                    opened: RefCell::new(Vec::new()),
                },
                _marker: PhantomData,
            },
            user_events,
        })
    }

    pub fn run<F>(mut self, mut handler: F) -> Result<(), EventLoopError>
    where
        F: FnMut(Event<T>, &event_loop::ActiveEventLoop),
    {
        wasdk::block_on(self.run_until_exit(&mut handler));
        Ok(())
    }

    async fn run_until_exit<F>(&mut self, handler: &mut F)
    where
        F: FnMut(Event<T>, &event_loop::ActiveEventLoop),
    {
        let mut start_cause = StartCause::Init;
        let mut arrival = None;
        loop {
            handler(Event::NewEvents(start_cause), &self.window_target);
            if start_cause == StartCause::Init {
                handler(Event::Resumed, &self.window_target);
            }
            // What woke the loop, then whatever else has arrived since.
            while let Some(arrived) = arrival.take() {
                self.adopt_opened_windows();
                self.deliver(arrived, handler);
                arrival = self.arrivals.ready().await;
            }
            handler(Event::AboutToWait, &self.window_target);
            self.adopt_opened_windows();
            if self.window_target.p.exiting() {
                break;
            }
            let start = Instant::now();
            (start_cause, arrival) = match self.window_target.p.control_flow() {
                ControlFlow::Poll => (StartCause::Poll, self.arrivals.ready().await),
                ControlFlow::Wait => {
                    let arrived = self.arrivals.next(None).await;
                    (StartCause::WaitCancelled { start, requested_resume: None }, Some(arrived))
                },
                ControlFlow::WaitUntil(requested_resume) => {
                    match self.arrivals.next(Some(requested_resume)).await {
                        Arrival::ResumeTime => {
                            (StartCause::ResumeTimeReached { start, requested_resume }, None)
                        },
                        arrived => {
                            let requested_resume = Some(requested_resume);
                            (StartCause::WaitCancelled { start, requested_resume }, Some(arrived))
                        },
                    }
                },
            };
        }
        handler(Event::LoopExiting, &self.window_target);
    }

    /// Starts reading the windows the app opened since the loop last looked.
    fn adopt_opened_windows(&mut self) {
        let opened = self.window_target.p.opened.take();
        self.arrivals.windows.extend(opened.into_iter().map(WindowEvents::of));
    }

    /// Hands `arrival` to the app as winit's events.
    fn deliver<F>(&mut self, arrival: Arrival<T>, handler: &mut F)
    where
        F: FnMut(Event<T>, &event_loop::ActiveEventLoop),
    {
        let target = &self.window_target;
        let mut to_window = |id: WindowId, event: WindowEvent| {
            handler(Event::WindowEvent { window_id: RootWindowId(id), event }, target);
        };
        match arrival {
            Arrival::View(id, Some(event)) => {
                let Some(window) = self.arrivals.windows.iter_mut().find(|window| window.id == id)
                else {
                    return;
                };
                for event in window.translate(event) {
                    to_window(id, event);
                }
            },
            Arrival::View(id, None) => {
                self.arrivals.windows.retain(|window| window.id != id);
                to_window(id, WindowEvent::Destroyed);
            },
            Arrival::System(SystemChange::PlatformBrightness(brightness)) => {
                for window in &self.arrivals.windows {
                    to_window(window.id, WindowEvent::ThemeChanged(theme_of(brightness)));
                }
            },
            Arrival::System(_) => {},
            Arrival::User(event) => handler(Event::UserEvent(event), target),
            Arrival::ResumeTime => {},
        }
    }

    pub fn window_target(&self) -> &event_loop::ActiveEventLoop {
        &self.window_target
    }

    pub fn create_proxy(&self) -> EventLoopProxy<T> {
        EventLoopProxy { user_events: self.user_events.clone() }
    }
}

pub struct EventLoopProxy<T: 'static> {
    user_events: UnboundedSender<T>,
}

impl<T> EventLoopProxy<T> {
    pub fn send_event(&self, event: T) -> Result<(), event_loop::EventLoopClosed<T>> {
        self.user_events
            .unbounded_send(event)
            .map_err(|refused| event_loop::EventLoopClosed(refused.into_inner()))
    }
}

impl<T> Clone for EventLoopProxy<T> {
    fn clone(&self) -> Self {
        EventLoopProxy { user_events: self.user_events.clone() }
    }
}

pub struct ActiveEventLoop {
    control_flow: Cell<ControlFlow>,
    exit: Cell<bool>,
    /// The windows opened since the loop last looked.
    opened: RefCell<Vec<OpenedWindow>>,
}

impl ActiveEventLoop {
    /// Hands the loop a window to read the events of.
    pub(super) fn opened(&self, window: OpenedWindow) {
        self.opened.borrow_mut().push(window);
    }

    pub fn create_custom_cursor(&self, source: CustomCursorSource) -> RootCustomCursor {
        let _ = source.inner;
        RootCustomCursor { inner: super::PlatformCustomCursor }
    }

    pub fn primary_monitor(&self) -> Option<MonitorHandle> {
        MonitorHandle::all().into_iter().next()
    }

    pub fn available_monitors(&self) -> VecDeque<MonitorHandle> {
        MonitorHandle::all().into()
    }

    #[inline]
    pub fn listen_device_events(&self, _allowed: DeviceEvents) {}

    #[inline]
    pub fn system_theme(&self) -> Option<Theme> {
        Some(theme_of(system::platform_brightness()))
    }

    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::RawDisplayHandle::Web(rwh_06::WebDisplayHandle::new()))
    }

    pub fn set_control_flow(&self, control_flow: ControlFlow) {
        self.control_flow.set(control_flow)
    }

    pub fn control_flow(&self) -> ControlFlow {
        self.control_flow.get()
    }

    pub(crate) fn exit(&self) {
        self.exit.set(true);
    }

    pub(crate) fn exiting(&self) -> bool {
        self.exit.get()
    }

    pub(crate) fn owned_display_handle(&self) -> OwnedDisplayHandle {
        OwnedDisplayHandle
    }
}

#[derive(Clone)]
pub(crate) struct OwnedDisplayHandle;

impl OwnedDisplayHandle {
    #[cfg(feature = "rwh_06")]
    #[inline]
    pub fn raw_display_handle_rwh_06(
        &self,
    ) -> Result<rwh_06::RawDisplayHandle, rwh_06::HandleError> {
        Ok(rwh_06::WebDisplayHandle::new().into())
    }
}
