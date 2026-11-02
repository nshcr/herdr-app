//! macOS input source memory follows terminal panes, never dialogs or other apps.

use super::*;

#[derive(Default)]
pub(super) struct InputSources {
    sources: HashMap<String, native::Source>,
    focused: Option<String>,
    pending: bool,
    restore: Option<Task<()>>,
    _events: Vec<Subscription>,
}

impl AppWindow {
    pub(super) fn watch_input_sources(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input_sources._events.push(cx.observe_window_activation(window, |this, window, cx| {
            this.sync_input_source(window, cx);
        }));
        let handle = window.window_handle();
        let this = cx.weak_entity();
        self.input_sources._events.push(cx.on_keyboard_layout_change(move |cx| {
            let _ = handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    let pane = this.input_source_pane(window, cx);
                    let state = &mut this.input_sources;
                    if window.is_window_active() && !state.pending && pane == state.focused
                        && let (Some(pane), Some(source)) = (pane, native::current())
                    {
                        state.sources.insert(pane, source);
                    }
                });
            });
        }));
    }

    pub(super) fn watch_pane_input_source(&self, focus: &FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> [Subscription; 2] {
        // The terminal's find input is a descendant of its focus handle.
        [
            cx.on_focus_in(focus, window, |this, window, cx| this.sync_input_source(window, cx)),
            cx.on_focus_out(focus, window, |this, _, window, cx| this.sync_input_source(window, cx)),
        ]
    }

    fn input_source_pane(&self, window: &Window, cx: &App) -> Option<String> {
        let (id, _) = self.terms.iter().find(|(_, term)| term.view.read(cx).owns_keyboard_focus(window, cx))?;
        let pane = id.strip_suffix(SWAP).unwrap_or(id);
        // Peeks, overlays and composition terminal blocks aren't tab panes.
        self.model.tab_of(pane).map(|_| pane.to_owned())
    }

    pub(super) fn sync_input_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = window.is_window_active().then(|| self.input_source_pane(window, cx)).flatten();
        let state = &mut self.input_sources;
        state.sources.retain(|pane, _| self.model.tab_of(pane).is_some());
        if pane == state.focused { return; }
        state.restore = None;
        state.pending = false;
        state.focused = pane.clone();
        let Some(pane) = pane else { return };
        // Notifications save the departing pane. Sampling it here could save
        // a source AppKit has already changed for the destination instead.
        let current = native::current();
        let source = match state.sources.get(&pane) {
            Some(saved) if Some(saved) != current.as_ref() => saved.clone(),
            Some(_) => return,
            None => {
                if let Some(current) = current { state.sources.insert(pane, current); }
                return;
            }
        };
        state.pending = true;
        // TIS can synchronously notify GPUI: select outside an App/entity update.
        state.restore = Some(cx.spawn_in(window, async move |this, cx| {
            let valid = this.update_in(cx, |this, window, cx| {
                window.is_window_active() && this.input_source_pane(window, cx).as_deref() == Some(&pane)
                    && this.input_sources.focused.as_deref() == Some(&pane)
            }).unwrap_or(false);
            if !valid { return; }
            if !native::select(&source) {
                tracing::debug!("could not restore pane input source");
            }
            let current = native::current();
            let _ = this.update_in(cx, |this, _, _| {
                let state = &mut this.input_sources;
                if state.focused.as_deref() == Some(&pane) {
                    state.pending = false;
                    if let Some(current) = current { state.sources.insert(pane, current); }
                }
            });
        }));
    }
}

mod native {
    use std::ffi::c_void;
    type Ref = *const c_void;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        fn TISCopyCurrentKeyboardInputSource() -> Ref;
        fn TISSelectInputSource(source: Ref) -> i32;
        fn CFRetain(value: Ref) -> Ref;
        fn CFRelease(value: Ref);
        fn CFEqual(a: Ref, b: Ref) -> u8;
    }

    // Own a non-null Copy/Retain result. This is the IME source, not its layout.
    pub(super) struct Source(Ref);
    impl Clone for Source {
        fn clone(&self) -> Self { Self(unsafe { CFRetain(self.0) }) }
    }
    impl PartialEq for Source {
        fn eq(&self, other: &Self) -> bool { unsafe { CFEqual(self.0, other.0) != 0 } }
    }
    impl Drop for Source {
        fn drop(&mut self) { unsafe { CFRelease(self.0) }; }
    }

    pub(super) fn current() -> Option<Source> {
        let source = unsafe { TISCopyCurrentKeyboardInputSource() };
        if source.is_null() { None } else { Some(Source(source)) }
    }

    pub(super) fn select(source: &Source) -> bool {
        current().as_ref() == Some(source) || unsafe { TISSelectInputSource(source.0) == 0 }
    }
}
