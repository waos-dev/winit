//! # WaOS
//!
//! A winit app as a WaOS app: a WASI component built for `wasm32-wasip3`.

use std::future::Future;

/// Starts `task` on the event loop, on the app's one thread: the loop runs it whenever it waits
/// for the next event, until the task ends or the loop does. A task started before the loop runs
/// starts with it.
///
/// This is how a winit app on WaOS runs work beside its loop. `wasdk::spawn` hands a task to
/// whichever wait on the thread runs next and makes that wait last until the task ends, so a
/// task started just before a window is opened, or inside any call that waits (`wasdk::block_on`,
/// which reading the clipboard or showing a file dialog also is), would hold that call for as
/// long as it runs.
pub fn spawn_local(task: impl Future<Output = ()> + 'static) {
    crate::platform_impl::spawn_local(task);
}
