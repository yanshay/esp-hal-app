use core::{cell::RefCell, slice};
use esp_hal::ledc::channel::ChannelIFace;

use alloc::{boxed::Box, rc::Rc};
use embassy_futures::select::{select3, select4, Either3, Either4};
use embassy_time::{Duration, Timer};
use esp_hal::{dma::DmaTxBuf, gpio::Input, lcd_cam::lcd::i8080::I8080Transfer, ledc::LowSpeed};
use slint::platform::{software_renderer::Rgb565Pixel, update_timers_and_animations, WindowEvent};

use crate::{
    framework::Framework,
    slint_ext::McuWindow,
    touch::{Touch, TouchEvent, TouchPosition},
};

// For collecting stats on rendering time split
static mut GRAPHICS_TOTAL: u64 = 0;
static mut TOTAL_LINES: u64 = 0;
static mut TOTAL_PIXELS: u64 = 0;

#[embassy_executor::task]
#[allow(clippy::too_many_arguments)]
pub async fn event_loop(
    ti_i2c: esp_hal::i2c::master::I2c<'static, esp_hal::Blocking>,
    ti_irq: Input<'static>,
    window: Rc<McuWindow>,
    mut buffer_provider: DrawBuffer<'static, esp_hal::Blocking>,
    mut channel0: esp_hal::ledc::channel::Channel<'static, LowSpeed>,
    lstimer0: &'static esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::LowSpeed>,
    size: slint::PhysicalSize,
    framework: Rc<RefCell<Framework>>,
) {
    let undim_display = framework.borrow().undim_display;
    // Turn on display backlight
    channel0
        .configure(esp_hal::ledc::channel::config::Config {
            timer: lstimer0,
            duty_pct: 100,
            pin_config: esp_hal::ledc::channel::config::PinConfig::PushPull,
        })
        .unwrap();

    let mut touch_inner = ft6x36::Ft6x36::new(ti_i2c, ft6x36::Dimension(319, 479));
    touch_inner.set_orientation(ft6x36::Orientation::Landscape);
    touch_inner.init().unwrap();

    let mut touch = Touch::new(touch_inner, ti_irq);

    // == Event Loop ==================================================================

    // https://github.com/slint-ui/slint/discussions/3994
    // https://slint.dev/releases/1.0.2/docs/rust/slint/docs/mcu/#the-event-loop
    // https://github.com/slint-ui/slint/issues/2793#issuecomment-1609154575

    // Process touch events as stream so not to restart the touch future every time from scratch
    // should be more efficient and also maybe avoid missing events

    use futures_util::StreamExt; // reuired since includes reuired implementation
    let mut touch_events_stream = Box::pin(touch.events_stream_async());

    // Helper function for coordinates transformation
    #[inline(always)]
    fn touch_pos_to_logical_pos(pos: TouchPosition, _size: &slint::PhysicalSize, window: &McuWindow) -> slint::LogicalPosition {
        slint::PhysicalPosition::new(pos.x as _, pos.y as _).to_logical(window.scale_factor())
    }

    // Helper function for turning sync function to cooperate with embassy async framework
    // async fn async_update_timers_and_animations() {
    //     slint::platform::update_timers_and_animations();
    //     embassy_futures::yield_now().await;
    // }

    // Touch events will translate to left button mouse
    let button = slint::platform::PointerEventButton::Left;

    let mut last_touch_time = embassy_time::Instant::now();
    let mut display_fully_dimmed = false;
    let mut display_partially_dimmed = false;
    let mut ignore_touch = false;

    // let mut loop_count = 0;
    loop {
        // loop_count += 1;
        // dbg!(loop_count);

        // draw at the beginning, for first time drawing, in case (common) will await following that
        slint::platform::update_timers_and_animations();

        window.draw_if_needed(|renderer| {
            let start_graphics_time = embassy_time::Instant::now();

            // For single line rendering (2/2)
            renderer.render_by_line(&mut buffer_provider);

            let graphics_time = start_graphics_time.elapsed();
            unsafe {
                GRAPHICS_TOTAL += graphics_time.as_micros();
            }
        });

        let async_res;

        if window.has_active_animations() {
            update_timers_and_animations();
            // async_res = Either3::Second(());
            // TODO: think how to deal with update timers and animations, even when nothing waked up event loop (due to backend changes, or maybe timers in slint?)
            //       I think I've done it, but keeping this to make sure I verify this
            let res = select3(touch_events_stream.next(), embassy_futures::yield_now(), undim_display.wait()).await;
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
                warn!("Shouldn't get here, event_stream_async should either wait or return an event");
            }
            Either4::First(_) | Either4::Fourth(_) => {
                // Start with common to touch and undim - need to undim the display
                last_touch_time = embassy_time::Instant::now();
                slint::platform::update_timers_and_animations();
                if display_partially_dimmed || display_fully_dimmed {
                    trace!("Undim display");
                    channel0
                        .configure(esp_hal::ledc::channel::config::Config {
                            timer: lstimer0,
                            duty_pct: 100,
                            pin_config: esp_hal::ledc::channel::config::PinConfig::PushPull,
                        })
                        .unwrap();
                    display_fully_dimmed = false;
                    display_partially_dimmed = false;
                }
                // Now address the case of touch
                if let Either4::First(Some(event)) = async_res {
                    match event {
                        // Ignore error because nothing much we can do about it
                        Err(_) => (),
                        Ok(event) => {
                            if let Some(event) = event {
                                match event {
                                    TouchEvent::TouchMoved(pos) => {
                                        if !ignore_touch {
                                            let position = touch_pos_to_logical_pos(pos, &size, &window);
                                            let win_event = WindowEvent::PointerMoved { position };
                                            // dbg!(&win_event);
                                            window.dispatch_event(win_event);
                                        }
                                    }
                                    TouchEvent::TouchPressed(pos) => {
                                        if !ignore_touch {
                                            let position = touch_pos_to_logical_pos(pos, &size, &window);
                                            let win_event = WindowEvent::PointerPressed { position, button };
                                            // dbg!(&win_event);
                                            window.dispatch_event(win_event);
                                        }
                                    }
                                    TouchEvent::TouchReleased(pos) => {
                                        if !ignore_touch {
                                            let position = touch_pos_to_logical_pos(pos, &size, &window);
                                            let win_event = WindowEvent::PointerReleased { position, button };
                                            // dbg!(&win_event);
                                            window.dispatch_event(win_event);
                                            window.dispatch_event(WindowEvent::PointerExited);
                                        } else {
                                            ignore_touch = false;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Either4::Second(_) => {
                let framework = framework.borrow();
                if !display_fully_dimmed && last_touch_time.elapsed().as_secs() > framework.display_blackout_timeout {
                    channel0
                        .configure(esp_hal::ledc::channel::config::Config {
                            timer: lstimer0,
                            duty_pct: 0,
                            pin_config: esp_hal::ledc::channel::config::PinConfig::PushPull,
                        })
                        .unwrap();
                    if !display_fully_dimmed {
                        info!("Darkening display")
                    }
                    display_fully_dimmed = true;
                    ignore_touch = true;
                } else if !display_partially_dimmed && last_touch_time.elapsed().as_secs() > framework.display_dimming_timeout {
                    trace!("Darken display");
                    channel0
                        .configure(esp_hal::ledc::channel::config::Config {
                            timer: lstimer0,
                            duty_pct: framework.display_dimming_percent,
                            pin_config: esp_hal::ledc::channel::config::PinConfig::PushPull,
                        })
                        .unwrap();
                    display_partially_dimmed = true;
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

// ===============================================================================================================
// == Slint Esp Backend Implementation for drawing and timer, specific to thid device ============================
// ===============================================================================================================

pub struct EspBackend<'a> {
    pub window: Rc<McuWindow>,
    pub rtc: Rc<esp_hal::rtc_cntl::Rtc<'a>>,
}

impl slint::platform::Platform for EspBackend<'_> {
    fn create_window_adapter(&self) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> core::time::Duration {
        core::time::Duration::from_millis(self.rtc.time_since_boot().to_millis())
    }
    fn debug_log(&self, arguments: core::fmt::Arguments) {
        debug!("{}", arguments);
    }
}

pub struct DrawBuffer<'a, DM>
where
    DM: esp_hal::DriverMode,
{
    pub dma_buf0: Option<DmaTxBuf>,
    pub dma_buf1: Option<DmaTxBuf>,
    pub dma_buf_cmd: Option<DmaTxBuf>,
    pub transfer: Option<I8080Transfer<'a, DmaTxBuf, DM>>,
    pub curr_buffer: usize,
    pub prev_range: core::ops::Range<usize>,
    pub prev_line: usize,
    #[allow(clippy::type_complexity)]
    pub i8080: Option<esp_hal::lcd_cam::lcd::i8080::I8080<'a, DM>>,
}

impl<DM> slint::platform::software_renderer::LineBufferProvider for &mut DrawBuffer<'_, DM>
where
    DM: esp_hal::DriverMode,
{
    type TargetPixel = Rgb565Pixel;

    fn process_line(
        &mut self,
        line: usize,
        range: core::ops::Range<usize>,
        render_fn: impl FnOnce(&mut [slint::platform::software_renderer::Rgb565Pixel]),
    ) {
        let mut dma_buf;
        let prev_dma_buf_id;
        if self.dma_buf0.is_some() {
            dma_buf = self.dma_buf0.take().unwrap();
            prev_dma_buf_id = 1;
        } else {
            dma_buf = self.dma_buf1.take().unwrap();
            prev_dma_buf_id = 0;
        }

        let pixels = range.end - range.start;

        let dma_buf_as_pixel_ptr: *mut Rgb565Pixel = dma_buf.as_mut_slice().as_mut_ptr() as *mut Rgb565Pixel;
        let buffer: &mut [Rgb565Pixel] = unsafe { slice::from_raw_parts_mut(dma_buf_as_pixel_ptr, pixels) };
        render_fn(buffer);
        dma_buf.set_length(pixels * core::mem::size_of::<Rgb565Pixel>());

        let mut i8080;
        if self.transfer.is_some() {
            let prev_dma_buf;
            (_, i8080, prev_dma_buf) = self.transfer.take().unwrap().wait();
            if prev_dma_buf_id == 0 {
                self.dma_buf0 = Some(prev_dma_buf);
            } else {
                self.dma_buf1 = Some(prev_dma_buf);
            }
        } else {
            i8080 = self.i8080.take().unwrap();
        }

        let mut data_cmd = 0x3cu8; // assume it's not the first line of a rectangle region, so command for next line
        if self.prev_range != range || line != self.prev_line + 1 {
            let mut dma_buf_cmd = self.dma_buf_cmd.take().unwrap();
            let range_start_b = range.start.to_be_bytes();
            let range_end_b = (range.end - 1).to_be_bytes();
            let cmdbuffer_h = [range_start_b[3], range_start_b[2], range_end_b[3], range_end_b[2]]; // working with fixed set_byte_order with correct colors
            dma_buf_cmd.fill(&cmdbuffer_h);
            let transfer = i8080.send(0x2au8, 0, dma_buf_cmd).unwrap();

            let line_start_b = line.to_be_bytes();
            let num_lines_b = 479u16.to_be_bytes();
            let cmdbuffer_v = [line_start_b[3], line_start_b[2], num_lines_b[1], num_lines_b[0]]; // working with fixed set_byte_order with correct colors

            (_, i8080, dma_buf_cmd) = transfer.wait(); // wait for end of previous (horizontal) transfer first - minor double buffering :-)

            dma_buf_cmd.fill(&cmdbuffer_v);
            let transfer = i8080.send(0x2bu8, 0, dma_buf_cmd).unwrap();
            (_, i8080, dma_buf_cmd) = transfer.wait();
            self.dma_buf_cmd = Some(dma_buf_cmd);

            self.prev_range = range;
            data_cmd = 0x2cu8; // it's a new region, so command for data should state it's a first line
        }
        self.prev_line = line;

        // Double buffering, wait will be only on next line after rendering
        self.transfer = Some(i8080.send(data_cmd, 0, dma_buf).unwrap());

        // No double buffering - complete dma now
        // (_, i8080, dma_buf) = i8080.send(data_cmd, 0, dma_buf).unwrap().wait();
        // self.i8080 = Some(i8080);
        // if prev_dma_buf_id == 0 {
        //     self.dma_buf1 = Some(dma_buf);
        // } else {
        //     self.dma_buf0 = Some(dma_buf);
        // }

        unsafe {
            TOTAL_LINES += 1;
            TOTAL_PIXELS += pixels as u64;
        }
    }
}

#[embassy_executor::task]
async fn stats_task() {
    loop {
        unsafe {
            dbg!(GRAPHICS_TOTAL, TOTAL_LINES, TOTAL_PIXELS);
        }
        Timer::after_secs(5).await;
    }
}

// == WT32-SC01 Fast Display Bus instead of slow display_interface_parallel_gpio bus ================================================================
// Not really needed since we use DMA now, so this is used only for setup, but may be useful for fast gpio in the future, so using this implementation

pub struct SC01DislpayOutputBus {}
const FAST: bool = true;
impl SC01DislpayOutputBus {
    pub fn new() -> Self {
        if FAST {
            Self::init();
        }
        SC01DislpayOutputBus {}
    }

    pub fn init() {
        unsafe { &*esp32s3::GPIO::PTR }.out1_w1tc().write(|w| unsafe { w.bits(0x04 << 13) });
        connect_gpio_to_fast_gpio_bit_core0(9, 0);
        connect_gpio_to_fast_gpio_bit_core0(46, 1);
        connect_gpio_to_fast_gpio_bit_core0(3, 2);
        connect_gpio_to_fast_gpio_bit_core0(8, 3);
        connect_gpio_to_fast_gpio_bit_core0(18, 4);
        connect_gpio_to_fast_gpio_bit_core0(17, 5);
        connect_gpio_to_fast_gpio_bit_core0(16, 6);
        connect_gpio_to_fast_gpio_bit_core0(15, 7);
        Self::out_u8_fast(0);
    }

    pub fn out_u8_fast(value: u8) {
        // gpio47 is wr, so we clear it at the beginning
        fast_gpio_out(value);

        unsafe { &*esp32s3::GPIO::PTR }.out1_w1ts().write(|w| unsafe { w.bits(0x04 << 13) });
        unsafe { &*esp32s3::GPIO::PTR }.out1_w1ts().write(|w| unsafe { w.bits(0x04 << 13) });
        unsafe { &*esp32s3::GPIO::PTR }.out1_w1ts().write(|w| unsafe { w.bits(0x04 << 13) });
        unsafe { &*esp32s3::GPIO::PTR }.out1_w1ts().write(|w| unsafe { w.bits(0x04 << 13) });

        unsafe { &*esp32s3::GPIO::PTR }.out1_w1tc().write(|w| unsafe { w.bits(0x04 << 13) });
    }

    pub fn _out_u8_fast_working(value: u8) {
        // with gpio 47 instead of 9
        // gpio47 is wr, so we clear it at the beginning
        let bits = value & 0xfe;
        fast_gpio_out(bits);

        for _ in 0..5 {
            if value & 0x01 != 0 {
                unsafe { &*esp32s3::GPIO::PTR }.out_w1ts().write(|w| unsafe { w.bits(0b1000000000) });
            } else {
                unsafe { &*esp32s3::GPIO::PTR }.out_w1tc().write(|w| unsafe { w.bits(0b1000000000) });
            }
        }

        let bits = value | 0x01;
        fast_gpio_out(bits);

        // unsafe { &*hal::peripherals::GPIO::PTR }.out1_w1ts().write(|w| unsafe { w.bits(0x04 << 13) });
    }

    pub fn out_u8(value: u8) {
        if FAST {
            Self::out_u8_fast(value);
        } else {
            Self::out_u8_slow(value);
        }
    }

    pub fn out_u8_slow(value: u8) {
        // bit 0 -> gpio9, so shift left 9
        // * bit 1 -> gpio46, so shift left 45-32=13 on the high set of gpios register
        // bit 2 -> gpio3, so shift left 1
        // bit 3 -> gpio8, so shift left 5
        // bit 4 -> gpio18, so shift left 14
        // bit 5 -> gpio17, so shift left 12
        // bit 6 -> gpio 16, so shift left 10
        // bit 7 -> gpio 15, so shift left 8

        // gpio47 is wr, so we clear it at the beginning, potentially together with gpio46 to save a potential extra write
        // it will be raise back at the end (but there, not together with 46 since it doesn't work due to race)

        let gpio46set = value & 0x02;
        if gpio46set == 0 {
            unsafe { &*esp32s3::GPIO::PTR }.out1_w1tc().write(|w| unsafe { w.bits(0x06 << 13) });
        } else {
            unsafe { &*esp32s3::GPIO::PTR }.out1_w1tc().write(|w| unsafe { w.bits(0x04 << 13) });
        }

        // Now handle the rest of bits/gpios

        let bits: u32 = value as u32;

        let gpio0to31set: u32 = ((bits & 0x01) << 9)
            | ((bits & 0x04) << 1)
            | ((bits & 0x08) << 5)
            | ((bits & 0x10) << 14)
            | ((bits & 0x20) << 12)
            | ((bits & 0x40) << 10)
            | ((bits & 0x80) << 8);
        let gpio0to31clear = (!gpio0to31set) & 0b00000000_00000111_10000011_00001000;

        if gpio0to31set != 0 {
            unsafe { &*esp32s3::GPIO::PTR }.out_w1ts().write(|w| unsafe { w.bits(gpio0to31set) });
        }
        if gpio0to31clear != 0 {
            unsafe { &*esp32s3::GPIO::PTR }.out_w1tc().write(|w| unsafe { w.bits(gpio0to31clear) });
        }

        // can't raise 46 together with 47, it doesn't capture 46 data bit
        if gpio46set != 0 {
            unsafe { &*esp32s3::GPIO::PTR }.out1_w1ts().write(|w| unsafe { w.bits(0x02 << 13) });
        } // the clear is done at the beginning together with 47, there it's ok

        // Now deal with gpio47 (wr signal)
        unsafe { &*esp32s3::GPIO::PTR }.out1_w1ts().write(|w| unsafe { w.bits(0x04 << 13) });
    }
}

impl display_interface_parallel_gpio::OutputBus for SC01DislpayOutputBus {
    type Word = u8;

    fn set_value(&mut self, value: Self::Word) -> Result<(), display_interface::DisplayError> {
        Self::out_u8(value);

        Ok(())
    }
}

pub fn connect_gpio_to_fast_gpio_bit_core0(gpio_num: usize, fast_gpio_bit: u16) {
    // GPIO_FUNCx_OUT_SEL_CFG
    let signal = 221 + fast_gpio_bit;
    unsafe { &*esp32s3::GPIO::PTR }
        .func_out_sel_cfg(gpio_num)
        .modify(|_, w| unsafe { w.out_sel().bits(signal).inv_sel().bit(false).oen_sel().bit(true).oen_inv_sel().bit(false) });

    if gpio_num > 31 {
        unsafe { &*esp32s3::GPIO::PTR }
            .enable1_w1ts()
            .write(|w| unsafe { w.enable1_w1ts().bits(0x1u32 << (gpio_num % 32)) });
    } else {
        unsafe { &*esp32s3::GPIO::PTR }
            .enable_w1ts()
            .write(|w| unsafe { w.enable_w1ts().bits(0x1u32 << (gpio_num)) });
    }

    // IO_MUX_MCU_SEL
    // unsafe { &*hal::peripherals::IO_MUX::PTR}.gpio[gpio_num].modify(|_, w| unsafe { w.mcu_sel().bits(1).fun_drv().bits(2) });

    unsafe { &*esp_hal::peripherals::IO_MUX::PTR }.gpio(gpio_num).modify(|_, w| unsafe {
        w.mcu_sel()
            .bits(1)
            .fun_ie()
            .bit(false)
            .fun_wpd()
            .clear_bit()
            .fun_wpu()
            .clear_bit()
            .fun_drv()
            .bits(2)
            .slp_sel()
            .clear_bit()
    });
}

#[inline(always)]
pub fn fast_gpio_out(data: u8) {
    let data: u32 = data as u32;
    let mask: u32 = 0xff;
    unsafe { core::arch::asm!("ee.wr_mask_gpio_out {0}, {1}", in(reg) data, in(reg) mask) };
}
