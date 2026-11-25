# winit for WaOS

This fork adds a WaOS backend to winit 0.30.13, so an app written for winit runs as a WaOS app: a WASI component built for `wasm32-wasip3`. The backend is `src/platform_impl/waos/`; everything else is upstream's. It is kept for whoever ports a winit app to WaOS, and is not offered upstream.

## Using it

An app takes the fork in place of winit, in its workspace's `Cargo.toml`:

```toml
[patch.crates-io]
winit = { git = "https://github.com/waos-dev/winit", branch = "waos" }
```

and builds as any WaOS app: `cargo build --target wasm32-wasip3`, or `waos run`.

Three things in the app's own code are WaOS's:

- **The wgpu instance** is `wasdk_wgpu::instance()` in place of `wgpu::Instance::new`. A window's handle is its view's, which that instance makes a surface from.
- **A wait on the app's thread** (for a wgpu adapter, say) is `wasdk::block_on`. An executor that parks the thread (`pollster`, `futures::executor::block_on`) never wakes: nothing else runs to wake it.
- **A task beside the loop** is `winit::platform::waos::spawn_local`, which the event loop runs whenever it waits. `wasdk::spawn` hands a task to whichever wait on the thread runs next and makes that wait last until the task ends, so a task that runs on, started inside a `wasdk::block_on` or just before a window is opened, would hold that call for good.

The fork asks for `wasdk` 0.7.1 with its `raw-window-handle` feature, which is on WaOS's `main` branch and not published yet. Until the next `wasdk` release an app also names `wasdk` from WaOS's repository in the same `[patch.crates-io]` (`wasdk = { git = "https://github.com/waos-dev/waos", branch = "main" }`), and `wasdk-wgpu` with it, so the fork and the app's drawing use the same one.

## What it does

- A window is one of WaOS's views with the window WaOS shows it in. `create_window` waits for the view, so it answers a window as on any system.
- The event loop runs on the app's one thread until `exit`, and wakes for a view's events, the system's light or dark changing, an `EventLoopProxy`'s event, or the time `ControlFlow::WaitUntil` names.
- Mouse, touch, trackpad gestures, keys with their modifiers, focus, size and scale, close requests and redraws arrive as winit's events. `request_redraw` asks WaOS for a frame.
- Title, size and position, size limits, maximize and minimize, full screen, visibility, the cursor's icon, dragging the window, the screens as monitors, and the theme.
- Full screen is the system's own for a window, on the monitor named or the window's own. winit has no event for it: a `Resized` follows, and `fullscreen()` answers how the window is. A view in no window, as on a phone, fills its screen as it is.

## What it leaves out

- **Text typed through the system's text input** (an input method, a phone's keyboard): `set_ime_allowed` does nothing and no `Ime` event is sent. Keys from a keyboard carry their text.
- **What WaOS sends and winit 0.30 has no event for**: the clipboard's and the Edit menu's commands (copy, cut, paste, undo), and the system's Back. A key's answer whether the app used it is always that it did.
- Custom cursors, the window's icon, grabbing or moving the cursor, `raw-window-handle` before 0.6.
