use alloc::{boxed::Box, rc::Rc};
use core::cell::RefCell;

use embassy_futures::select::{Either3, Either4, select3, select4};
use embassy_time::{Duration, Timer};
use slint::platform::{WindowEvent, update_timers_and_animations};

use crate::{
    backlight::{BacklightConfig, BacklightController, BacklightDevice},
    framework::Framework,
    slint_ext::McuWindow,
    touch::{Touch, TouchAdapter, TouchEvent, TouchPosition},
};

pub trait UiRenderBackend {
    fn render(&mut self, renderer: &slint::platform::software_renderer::SoftwareRenderer);
}

pub async fn event_loop<T, R, B>(
    mut touch: Touch<T>,
    window: Rc<McuWindow>,
    mut render_backend: R,
    mut backlight: B,
    framework: Rc<RefCell<Framework>>,
) where
    T: TouchAdapter,
    R: UiRenderBackend,
    B: BacklightDevice,
    B::Error: core::fmt::Debug,
{
    // == Event Loop ==================================================================

    // https://github.com/slint-ui/slint/discussions/3994
    // https://slint.dev/releases/1.0.2/docs/rust/slint/docs/mcu/#the-event-loop
    // https://github.com/slint-ui/slint/issues/2793#issuecomment-1609154575

    // Process touch events as stream so not to restart the touch future every time from scratch
    // should be more efficient and also maybe avoid missing events

    use futures_util::StreamExt; // reuired since includes reuired implementation
    let mut touch_events_stream = Box::pin(touch.events_stream_async());

    let undim_display = framework.borrow().undim_display;
    let mut backlight_controller = BacklightController::new();

    // Helper function for coordinates transformation
    #[inline(always)]
    fn touch_pos_to_logical_pos(pos: TouchPosition, window: &McuWindow) -> slint::LogicalPosition {
        slint::PhysicalPosition::new(pos.x as _, pos.y as _).to_logical(window.scale_factor())
    }

    // Helper function for turning sync function to cooperate with embassy async framework
    // async fn async_update_timers_and_animations() {
    //     slint::platform::update_timers_and_animations();
    //     embassy_futures::yield_now().await;
    // }

    // Touch events will translate to left button mouse
    let button = slint::platform::PointerEventButton::Left;

    // let mut loop_count = 0;
    loop {
        // loop_count += 1;
        // dbg!(loop_count);

        // draw at the beginning, for first time drawing, in case (common) will await following that
        slint::platform::update_timers_and_animations();

        window.draw_if_needed(|renderer| {
            render_backend.render(renderer);
        });

        let async_res;

        if window.has_active_animations() {
            update_timers_and_animations();
            // async_res = Either3::Second(());
            // TODO: think how to deal with update timers and animations, even when nothing waked up event loop (due to backend changes, or maybe timers in slint?)
            //       I think I've done it, but keeping this to make sure I verify this
            let res = select3(
                touch_events_stream.next(),
                embassy_futures::yield_now(),
                undim_display.wait(),
            )
            .await;
            match res {
                Either3::First(event) => {
                    async_res = Either4::First(event);
                }
                Either3::Second(_) => {
                    async_res = Either4::Second(());
                }
                Either3::Third(_) => {
                    async_res = Either4::Fourth(());
                }
            }
            update_timers_and_animations();
        } else {
            update_timers_and_animations();
            let wait_duration;
            if let Some(duration) = slint::platform::duration_until_next_timer_update() {
                wait_duration = Duration::from_micros(duration.as_micros() as u64);
            } else {
                wait_duration = Duration::from_micros(5_000_000); // can also be infinite, just for life check
            }

            async_res = select4(
                touch_events_stream.next(),
                Timer::after(wait_duration),
                window.wait_needs_redraw(),
                undim_display.wait(),
            )
            .await;
            slint::platform::update_timers_and_animations();
        }

        match async_res {
            Either4::First(None) => {
                warn!(
                    "Shouldn't get here, event_stream_async should either wait or return an event"
                );
            }
            Either4::First(_) | Either4::Fourth(_) => {
                // Start with common to touch and undim - need to undim the display
                slint::platform::update_timers_and_animations();
                if backlight_controller.is_partially_dimmed() || backlight_controller.is_fully_dimmed() {
                    trace!("Undimming the display");
                }
                backlight_controller
                    .register_activity(&mut backlight)
                    .expect("Failed to undim display backlight");

                // Now address the case of touch
                if let Either4::First(Some(event)) = async_res {
                    match event {
                        // Ignore error because nothing much we can do about it
                        Err(_) => panic!("Touch event stream failed"),
                        Ok(event) => {
                            if let Some(event) = event {
                                match event {
                                    TouchEvent::TouchMoved(pos) => {
                                        if !backlight_controller.ignoring_touch() {
                                            let position = touch_pos_to_logical_pos(pos, &window);
                                            let win_event = WindowEvent::PointerMoved { position };
                                            // dbg!(&win_event);
                                            window.dispatch_event(win_event);
                                        }
                                    }
                                    TouchEvent::TouchPressed(pos) => {
                                        if !backlight_controller.ignoring_touch() {
                                            let position = touch_pos_to_logical_pos(pos, &window);
                                            let win_event =
                                                WindowEvent::PointerPressed { position, button };
                                            // dbg!(&win_event);
                                            window.dispatch_event(win_event);
                                        }
                                    }
                                    TouchEvent::TouchReleased(pos) => {
                                        if !backlight_controller.ignoring_touch() {
                                            let position = touch_pos_to_logical_pos(pos, &window);
                                            let win_event =
                                                WindowEvent::PointerReleased { position, button };
                                            // dbg!(&win_event);
                                            window.dispatch_event(win_event);
                                            window.dispatch_event(WindowEvent::PointerExited);
                                        } else {
                                            backlight_controller.clear_ignore_touch();
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Either4::Second(_) => {
                let cfg = {
                    let framework = framework.borrow();
                    BacklightConfig {
                        dimming_timeout_secs: framework.display_dimming_timeout,
                        dimming_percent: framework.display_dimming_percent,
                        blackout_timeout_secs: framework.display_blackout_timeout,
                    }
                };

                let was_fully_dimmed = backlight_controller.is_fully_dimmed();
                let was_partially_dimmed = backlight_controller.is_partially_dimmed();
                backlight_controller
                    .tick(&mut backlight, cfg)
                    .expect("Failed to set display backlight dimming state");

                if !was_fully_dimmed && backlight_controller.is_fully_dimmed() {
                    info!("Blanking the display");
                } else if !was_partially_dimmed && backlight_controller.is_partially_dimmed() {
                    trace!("Dimming the display");
                }
                // Case of slint timeout
                // slint::platform::update_timers_and_animations();
            }
            Either4::Third(_) => {
                // Case of need to redraw
                // slint::platform::update_timers_and_animations();
            }
        }
    }
}
