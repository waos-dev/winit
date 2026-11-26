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

The fork asks for the published `wasdk` (0.8.0) with its `raw-window-handle` feature; an app draws with `wasdk-wgpu` of the same release, so the fork and the app's drawing use the same `wasdk`.

## What it does

- A window is one of WaOS's views with the window WaOS shows it in. `create_window` waits for the view, so it answers a window as on any system.
- The event loop runs on the app's one thread until `exit`, and wakes for a view's events, the system's light or dark changing, an `EventLoopProxy`'s event, or the time `ControlFlow::WaitUntil` names.
- Mouse, touch, trackpad gestures, keys with their modifiers, focus, size and scale, close requests and redraws arrive as winit's events. `request_redraw` asks WaOS for a frame.
- Title, size and position, size limits, maximize and minimize, full screen, visibility, the cursor's icon, dragging the window, the screens as monitors, and the theme.
- Full screen is the system's own for a window, on the monitor named or the window's own. winit has no event for it: a `Resized` follows, and `fullscreen()` answers how the window is. A view in no window, as on a phone, fills its screen as it is.
- Input methods: while the app allows the IME (`set_ime_allowed`), the window's view holds WaOS's text input, and what an input method composes and commits arrives as `Ime::Preedit` and `Ime::Commit`, with `Ime::Enabled` before the first. As on macOS's winit, a character typed with nothing composed arrives as its key, with its text, so an app that reads both types it once. `set_ime_purpose` and `set_ime_cursor_area` reach WaOS, the latter placing the candidate window. WaOS's text input holds an empty field: each value an input method sends is turned into the step it is, and the field is told empty again once nothing is composed.

## What it leaves out

- **What an input method does to the text around the caret**: WaOS's field is empty between compositions, so a phone keyboard's suggestions that rewrite a word, or its backspace with nothing composed, have nothing to act on. Keys from a keyboard still delete.
- **What WaOS sends and winit 0.30 has no event for**: the clipboard's and the Edit menu's commands (copy, cut, paste, undo), an input method's action (Enter arrives as its key), and the system's Back. A key's answer whether the app used it is always that it did.
- Custom cursors, the window's icon, grabbing or moving the cursor, `raw-window-handle` before 0.6.
