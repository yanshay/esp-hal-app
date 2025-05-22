// -------------- Slint McuWindow based on MinimalSoftwareWindow ----------------
// Added support for async signaling and waiting on need to draw due to backend driven change such
// as property change
// Only thing that had to be removed from MinimalSoftwareWindow is something to do with metrics,
// doesn't seem important (from the name)
// Important: MinimalSoftwareWindow needs to be copied and can't be wrapped and direct calls to it
// because of it's relations to the window it further contains. That one back reference to
// MinimalSoftwareWindow and so doesn't call the request_redraw of McuWindow but rather
// MinimalSoftwareWindow which is the entire point of this new McuWindow.

use alloc::rc::Rc;
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};

pub struct McuWindow {
    window: slint::Window,
    renderer: slint::platform::software_renderer::SoftwareRenderer,
    needs_redraw: core::cell::Cell<bool>,
    size: core::cell::Cell<slint::PhysicalSize>,
    redraw_signal: Signal<CriticalSectionRawMutex, u32>,
}

impl McuWindow {
    /// Instantiate a new MinimalWindowAdaptor
    ///
    /// The `repaint_buffer_type` parameter specify what kind of buffer are passed to the [`SoftwareRenderer`]
    pub fn new(
        repaint_buffer_type: slint::platform::software_renderer::RepaintBufferType,
    ) -> Rc<Self> {
        Rc::new_cyclic(|w: &alloc::rc::Weak<Self>| Self {
            window: slint::Window::new(w.clone()),
            renderer:
                slint::platform::software_renderer::SoftwareRenderer::new_with_repaint_buffer_type(
                    repaint_buffer_type,
                ),
            needs_redraw: Default::default(),
            size: Default::default(),
            redraw_signal: Signal::new(),
        })
    }
    /// If the window needs to be redrawn, the callback will be called with the
    /// [renderer](SoftwareRenderer) that should be used to do the drawing.
    ///
    /// [`SoftwareRenderer::render()`] or [`SoftwareRenderer::render_by_line()`] should be called
    /// in that callback.
    ///
    /// Return true if something was redrawn.
    pub fn draw_if_needed(
        &self,
        render_callback: impl FnOnce(&slint::platform::software_renderer::SoftwareRenderer),
    ) -> bool {
        if self.needs_redraw.replace(false) {
            render_callback(&self.renderer);
            true
        } else {
            false
        }
    }

    #[doc(hidden)]
    /// Forward to the window through Deref
    /// (Before 1.1, WindowAdapter didn't have set_size, so the one from Deref was used.
    /// But in Slint 1.1, if one had imported the WindowAdapter trait, the other one would be found)
    pub fn set_size(&self, size: impl Into<slint::WindowSize>) {
        self.window.set_size(size);
    }

    pub async fn wait_needs_redraw(&self) {
        self.redraw_signal.wait().await;
    }
}

impl slint::platform::WindowAdapter for McuWindow {
    fn window(&self) -> &slint::Window {
        &self.window
    }

    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }

    fn size(&self) -> slint::PhysicalSize {
        self.size.get()
    }
    fn set_size(&self, size: slint::WindowSize) {
        self.size.set(size.to_physical(1.));
        self.window
            .dispatch_event(slint::platform::WindowEvent::Resized {
                size: size.to_logical(1.),
            })
    }

    fn request_redraw(&self) {
        self.needs_redraw.set(true);
        self.redraw_signal.signal(1);
        // This is required for rust driven animated properties (when an animated property is set
        // by rust). Without this, when no user interaction, animated properties will not animate
        // and only jump straight to end value
        // https://github.com/slint-ui/slint/discussions/3994#discussioncomment-7667717

        // TODO: I removed this due to the use in this app of SwipeGestureHandler which caused panics.
        // This means that if backend processes need to trigger animations it won't happen w/o special code prior
        // making the property modification to update_timers_and_animations().
        // Can read more about it in https://github.com/slint-ui/slint/issues/6332
        // Need to find an alternative solution, may be required a change in slint
        // slint::platform::update_timers_and_animations();
    }
}

impl core::ops::Deref for McuWindow {
    type Target = slint::Window;
    fn deref(&self) -> &Self::Target {
        &self.window
    }
}
