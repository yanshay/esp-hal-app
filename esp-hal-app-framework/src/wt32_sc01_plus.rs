use alloc::{boxed::Box, rc::Rc, string::String};
use core::{cell::RefCell, slice};
use embassy_futures::select::{select3, select4, Either3, Either4};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::{Duration, Timer};
use esp_hal::{
    dma::DmaTxBuf,
    dma_buffers,
    gpio::{GpioPin, Input, Level, Output, Pull},
    lcd_cam::lcd::i8080::I8080Transfer,
    ledc::{channel::ChannelIFace, timer::TimerIFace, LowSpeed},
    peripherals::LCD_CAM,
    time::RateExtU32,
};
use mipidsi::models::ST7796;
use slint::platform::{software_renderer::Rgb565Pixel, update_timers_and_animations, WindowEvent};

use crate::{
    framework::Framework,
    mk_static,
    slint_ext::McuWindow,
    touch::{Touch, TouchEvent, TouchPosition},
};

// For collecting stats on rendering time split
static mut GRAPHICS_TOTAL: u64 = 0;
static mut TOTAL_LINES: u64 = 0;
static mut TOTAL_PIXELS: u64 = 0;

#[allow(clippy::too_many_arguments)]
pub async fn event_loop<I2C: embedded_hal::i2c::I2c> (
    touch_inner: ft6x36::Ft6x36<I2C>,
    ti_irq: Input<'static>,
    window: Rc<McuWindow>,
    mut buffer_provider: DrawBuffer<'static, esp_hal::Blocking>,
    mut channel0: esp_hal::ledc::channel::Channel<'static, LowSpeed>,
    lstimer0: &'static esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::LowSpeed>,
    size: slint::PhysicalSize,
    framework: Rc<RefCell<Framework>>,
) {
    let undim_display = framework.borrow().undim_display;

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
    fn touch_pos_to_logical_pos(
        pos: TouchPosition,
        _size: &slint::PhysicalSize,
        window: &McuWindow,
    ) -> slint::LogicalPosition {
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
                                            let position =
                                                touch_pos_to_logical_pos(pos, &size, &window);
                                            let win_event = WindowEvent::PointerMoved { position };
                                            // dbg!(&win_event);
                                            window.dispatch_event(win_event);
                                        }
                                    }
                                    TouchEvent::TouchPressed(pos) => {
                                        if !ignore_touch {
                                            let position =
                                                touch_pos_to_logical_pos(pos, &size, &window);
                                            let win_event =
                                                WindowEvent::PointerPressed { position, button };
                                            // dbg!(&win_event);
                                            window.dispatch_event(win_event);
                                        }
                                    }
                                    TouchEvent::TouchReleased(pos) => {
                                        if !ignore_touch {
                                            let position =
                                                touch_pos_to_logical_pos(pos, &size, &window);
                                            let win_event =
                                                WindowEvent::PointerReleased { position, button };
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
                if !display_fully_dimmed
                    && last_touch_time.elapsed().as_secs() > framework.display_blackout_timeout
                {
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
                } else if !display_partially_dimmed
                    && last_touch_time.elapsed().as_secs() > framework.display_dimming_timeout
                {
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

pub struct EspBackend {
    pub window: Rc<McuWindow>,
}

impl slint::platform::Platform for EspBackend {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> core::time::Duration {
        core::time::Duration::from_micros(esp_hal::time::now().ticks())
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

        let dma_buf_as_pixel_ptr: *mut Rgb565Pixel =
            dma_buf.as_mut_slice().as_mut_ptr() as *mut Rgb565Pixel;
        let buffer: &mut [Rgb565Pixel] =
            unsafe { slice::from_raw_parts_mut(dma_buf_as_pixel_ptr, pixels) };
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
            let cmdbuffer_h = [
                range_start_b[3],
                range_start_b[2],
                range_end_b[3],
                range_end_b[2],
            ]; // working with fixed set_byte_order with correct colors
            dma_buf_cmd.fill(&cmdbuffer_h);
            let transfer = i8080.send(0x2au8, 0, dma_buf_cmd).unwrap();

            let line_start_b = line.to_be_bytes();
            let num_lines_b = 479u16.to_be_bytes();
            let cmdbuffer_v = [
                line_start_b[3],
                line_start_b[2],
                num_lines_b[1],
                num_lines_b[0],
            ]; // working with fixed set_byte_order with correct colors

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

#[allow(non_snake_case)]
pub struct WT32SC01PlusPeripherals<C, P>
where
    C: esp_hal::peripheral::Peripheral<P: esp_hal::dma::TxChannelFor<LCD_CAM>> + 'static,
    P: esp_hal::peripheral::Peripheral<P: esp_hal::i2c::master::Instance> + 'static,
{
    pub GPIO47: GpioPin<47>,
    pub GPIO0: GpioPin<0>,
    pub GPIO45: GpioPin<45>,
    pub GPIO4: GpioPin<4>,
    pub LCD_CAM: LCD_CAM,
    pub GPIO9: GpioPin<9>,
    pub GPIO46: GpioPin<46>,
    pub GPIO3: GpioPin<3>,
    pub GPIO8: GpioPin<8>,
    pub GPIO18: GpioPin<18>,
    pub GPIO17: GpioPin<17>,
    pub GPIO16: GpioPin<16>,
    pub GPIO15: GpioPin<15>,
    pub LEDC: esp_hal::peripherals::LEDC,
    pub GPIO5: GpioPin<5>,
    pub GPIO6: GpioPin<6>,
    pub GPIO7: GpioPin<7>,
    pub DMA_CHx: C,
    pub I2Cx: P,
}

type InitDone = Signal<CriticalSectionRawMutex, Result<(), String>>;

pub struct WT32SC01Plus {
    init_done: &'static InitDone,
}

impl WT32SC01Plus {
    pub fn new<C, P>(
        peripherals: WT32SC01PlusPeripherals<C, P>,
        display_orientation: mipidsi::options::Orientation,
        framework: Rc<RefCell<Framework>>,
    ) -> (Self, WT32SC01PlusRunner<C, P>)
    where
        C: esp_hal::peripheral::Peripheral<P: esp_hal::dma::TxChannelFor<LCD_CAM>> + 'static,
        P: esp_hal::peripheral::Peripheral<P: esp_hal::i2c::master::Instance> + 'static,
    {
        let init_done = mk_static!(InitDone, InitDone::new());
        let runner = WT32SC01PlusRunner {
            peripherals: Some(peripherals),
            display_orientation,
            framework,
            init_done,
        };
        let me = Self { init_done };
        (me, runner)
    }
    pub async fn wait_init_done(&self) -> Result<(), String> {
        self.init_done.wait().await
    }
}

pub struct WT32SC01PlusRunner<C, P>
where
    C: esp_hal::peripheral::Peripheral<P: esp_hal::dma::TxChannelFor<LCD_CAM>> + 'static,
    P: esp_hal::peripheral::Peripheral<P: esp_hal::i2c::master::Instance> + 'static,
{
    peripherals: Option<WT32SC01PlusPeripherals<C, P>>,
    display_orientation: mipidsi::options::Orientation,
    framework: Rc<RefCell<Framework>>,
    init_done: &'static InitDone,
}

impl<C, P> WT32SC01PlusRunner<C, P>
where
    C: esp_hal::peripheral::Peripheral<P: esp_hal::dma::TxChannelFor<LCD_CAM>> + 'static,
    P: esp_hal::peripheral::Peripheral<P: esp_hal::i2c::master::Instance> + 'static,
{
    pub async fn run(&mut self) {
        let mut peripherals = self.peripherals.take().unwrap();

        // == Setup Display Interface (di) ================================================

        let di_wr = Output::new(&mut peripherals.GPIO47, Level::High);
        let di_dc = Output::new(&mut peripherals.GPIO0, Level::High);
        let di_bl = peripherals.GPIO45;
        let di_rst = Output::new(peripherals.GPIO4, Level::High);

        let fastbus = SC01DislpayOutputBus::new();
        let di = display_interface_parallel_gpio::PGPIO8BitInterface::new(fastbus, di_dc, di_wr);

        // Initialize display using standard mipidsi dislay driver, then switch to faster display method for screen data
        let display = mipidsi::Builder::new(ST7796, di)
            .display_size(320, 480)
            .invert_colors(mipidsi::options::ColorInversion::Inverted)
            .color_order(mipidsi::options::ColorOrder::Bgr)
            .orientation(self.display_orientation)
            .reset_pin(di_rst)
            // .init(&mut delay)
            .init(&mut esp_hal::delay::Delay::new())
            .unwrap();

        let (di, _model, _rst) = display.release();
        let (_bus, _di_dc, _di_wr) = di.release();

        // Display initialization is done, now switch to LCD_CAM/DMA for driving data fast to the display

        let lcd_cam = esp_hal::lcd_cam::LcdCam::new(peripherals.LCD_CAM);

        let tx_pins = esp_hal::lcd_cam::lcd::i8080::TxEightBits::new(
            peripherals.GPIO9,
            peripherals.GPIO46,
            peripherals.GPIO3,
            peripherals.GPIO8,
            peripherals.GPIO18,
            peripherals.GPIO17,
            peripherals.GPIO16,
            peripherals.GPIO15,
        );

        let di_wr = peripherals.GPIO47;
        let di_dc = peripherals.GPIO0;

        let mut i8080_config = esp_hal::lcd_cam::lcd::i8080::Config::default();
        i8080_config.frequency = 40.MHz();

        let mut i8080 = esp_hal::lcd_cam::lcd::i8080::I8080::new(
            lcd_cam.lcd,
            peripherals.DMA_CHx,
            tx_pins,
            i8080_config,
        )
        .unwrap()
        .with_ctrl_pins(di_dc, di_wr);
        i8080.set_8bits_order(esp_hal::lcd_cam::ByteOrder::Inverted);

        let (_, _, tx_buffer0, tx_descriptors0) = dma_buffers!(
            0,
            480 * core::mem::size_of::<slint::platform::software_renderer::Rgb565Pixel>()
        );
        let (_, _, tx_buffer1, tx_descriptors1) = dma_buffers!(
            0,
            480 * core::mem::size_of::<slint::platform::software_renderer::Rgb565Pixel>()
        );
        let dma_buf0 = DmaTxBuf::new(tx_descriptors0, tx_buffer0).unwrap();
        let dma_buf1 = DmaTxBuf::new(tx_descriptors1, tx_buffer1).unwrap();

        let (_, _, tx_buffer_cmd, tx_descriptors_cmd) = dma_buffers!(0, 4);
        let dma_buf_cmd = DmaTxBuf::new(tx_descriptors_cmd, tx_buffer_cmd).unwrap();

        let buffer_provider = DrawBuffer {
            i8080: Some(i8080),
            dma_buf0: Some(dma_buf0),
            dma_buf1: Some(dma_buf1),
            dma_buf_cmd: Some(dma_buf_cmd),
            transfer: None,
            curr_buffer: 0,
            prev_range: core::ops::Range::<usize> {
                start: 10000,
                end: 10000,
            },
            prev_line: 0,
        };

        // Initialize backlight pwm control
        let mut ledc = esp_hal::ledc::Ledc::new(peripherals.LEDC);
        ledc.set_global_slow_clock(esp_hal::ledc::LSGlobalClkSource::APBClk);
        let lstimer0: &mut esp_hal::ledc::timer::Timer<esp_hal::ledc::LowSpeed> = mk_static!(
            esp_hal::ledc::timer::Timer<esp_hal::ledc::LowSpeed>,
            ledc.timer::<esp_hal::ledc::LowSpeed>(esp_hal::ledc::timer::Number::Timer0)
        );
        lstimer0
            .configure(esp_hal::ledc::timer::config::Config {
                duty: esp_hal::ledc::timer::config::Duty::Duty5Bit,
                clock_source: esp_hal::ledc::timer::LSClockSource::APBClk,
                frequency: 24u32.kHz(),
            })
            .unwrap();
        let mut channel0 = ledc.channel(esp_hal::ledc::channel::Number::Channel0, di_bl);

        // == Setup Touch Interface =======================================================

        let ti_sda = peripherals.GPIO6; //.into_push_pull_output();
        let ti_scl = peripherals.GPIO5; //.into_push_pull_output();
        let ti_irq = Input::new(peripherals.GPIO7, Pull::Down); //.into_push_pull_output();

        // TODO: Check the option of switching to async I2C instead of my own interrupt approach
        // let _ti_i2c = esp_hal::i2c::master::I2c::new(peripherals.I2C0, {
        //     let mut config = esp_hal::i2c::master::Config::default();
        //     config.frequency = 400u32.kHz();
        //     config
        // });

        let ti_i2c = esp_hal::i2c::master::I2c::new(
            peripherals.I2Cx,
            esp_hal::i2c::master::Config::default().with_frequency(400.kHz()),
        )
        .unwrap()
        .with_sda(ti_sda)
        .with_scl(ti_scl);

        esp_hal::interrupt::enable(
            esp_hal::peripherals::Interrupt::GPIO,
            esp_hal::interrupt::Priority::Priority3,
        )
        .unwrap();

        // == Setup the Slint Bacdkend ====================================================

        let (width, height, ft6x36orientation) = match self.display_orientation.rotation {
            mipidsi::options::Rotation::Deg0 => (320, 480, ft6x36::Orientation::Portrait), // ?? orientation not tested
            mipidsi::options::Rotation::Deg180 => (320, 480, ft6x36::Orientation::InvertedPortrait), // ?? orientation not tested
            mipidsi::options::Rotation::Deg90 => (480, 320, ft6x36::Orientation::Landscape),
            mipidsi::options::Rotation::Deg270 => (480, 320, ft6x36::Orientation::InvertedLandscape),
        };

        let size = slint::PhysicalSize::new(width, height);
        let window =
            McuWindow::new(slint::platform::software_renderer::RepaintBufferType::ReusedBuffer);
        window.set_size(size);
        slint::platform::set_platform(Box::new(EspBackend {
            window: window.clone(),
        }))
        .expect("backend already initialized");

        let mut touch_inner = ft6x36::Ft6x36::new(ti_i2c, ft6x36::Dimension((height-1) as u16, (width -1) as u16));
        touch_inner.set_orientation(ft6x36orientation);
        touch_inner.init().unwrap();

        // Turn on display backlight
        channel0
            .configure(esp_hal::ledc::channel::config::Config {
                timer: lstimer0,
                duty_pct: 100,
                pin_config: esp_hal::ledc::channel::config::PinConfig::PushPull,
            })
            .unwrap();


        self.init_done.signal(Ok(()));

        event_loop(
            touch_inner,
            ti_irq,
            window,
            buffer_provider,
            channel0,
            lstimer0,
            size,
            self.framework.clone(),
        )
        .await;
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
        unsafe { &*esp32s3::GPIO::PTR }
            .out1_w1tc()
            .write(|w| unsafe { w.bits(0x04 << 13) });
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

        unsafe { &*esp32s3::GPIO::PTR }
            .out1_w1ts()
            .write(|w| unsafe { w.bits(0x04 << 13) });
        unsafe { &*esp32s3::GPIO::PTR }
            .out1_w1ts()
            .write(|w| unsafe { w.bits(0x04 << 13) });
        unsafe { &*esp32s3::GPIO::PTR }
            .out1_w1ts()
            .write(|w| unsafe { w.bits(0x04 << 13) });
        unsafe { &*esp32s3::GPIO::PTR }
            .out1_w1ts()
            .write(|w| unsafe { w.bits(0x04 << 13) });

        unsafe { &*esp32s3::GPIO::PTR }
            .out1_w1tc()
            .write(|w| unsafe { w.bits(0x04 << 13) });
    }

    pub fn _out_u8_fast_working(value: u8) {
        // with gpio 47 instead of 9
        // gpio47 is wr, so we clear it at the beginning
        let bits = value & 0xfe;
        fast_gpio_out(bits);

        for _ in 0..5 {
            if value & 0x01 != 0 {
                unsafe { &*esp32s3::GPIO::PTR }
                    .out_w1ts()
                    .write(|w| unsafe { w.bits(0b1000000000) });
            } else {
                unsafe { &*esp32s3::GPIO::PTR }
                    .out_w1tc()
                    .write(|w| unsafe { w.bits(0b1000000000) });
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
            unsafe { &*esp32s3::GPIO::PTR }
                .out1_w1tc()
                .write(|w| unsafe { w.bits(0x06 << 13) });
        } else {
            unsafe { &*esp32s3::GPIO::PTR }
                .out1_w1tc()
                .write(|w| unsafe { w.bits(0x04 << 13) });
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
            unsafe { &*esp32s3::GPIO::PTR }
                .out_w1ts()
                .write(|w| unsafe { w.bits(gpio0to31set) });
        }
        if gpio0to31clear != 0 {
            unsafe { &*esp32s3::GPIO::PTR }
                .out_w1tc()
                .write(|w| unsafe { w.bits(gpio0to31clear) });
        }

        // can't raise 46 together with 47, it doesn't capture 46 data bit
        if gpio46set != 0 {
            unsafe { &*esp32s3::GPIO::PTR }
                .out1_w1ts()
                .write(|w| unsafe { w.bits(0x02 << 13) });
        } // the clear is done at the beginning together with 47, there it's ok

        // Now deal with gpio47 (wr signal)
        unsafe { &*esp32s3::GPIO::PTR }
            .out1_w1ts()
            .write(|w| unsafe { w.bits(0x04 << 13) });
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
        .modify(|_, w| unsafe {
            w.out_sel()
                .bits(signal)
                .inv_sel()
                .bit(false)
                .oen_sel()
                .bit(true)
                .oen_inv_sel()
                .bit(false)
        });

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

    unsafe { &*esp_hal::peripherals::IO_MUX::PTR }
        .gpio(gpio_num)
        .modify(|_, w| unsafe {
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
