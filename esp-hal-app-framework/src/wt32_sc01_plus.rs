use alloc::{boxed::Box, rc::Rc, string::String};
use core::{cell::RefCell, slice};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::Timer;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{
    dma::DmaTxBuf,
    dma_buffers,
    gpio::{Input, InputConfig, Level, Output, OutputConfig, Pull},
    lcd_cam::{
        LcdCam, lcd::i8080::I8080Transfer
    },
    ledc::{LowSpeed, channel::ChannelIFace, timer::TimerIFace},
    peripherals::LCD_CAM,
    spi,
    time::Rate,
};
use mipidsi::models::ST7796;
use slint::platform::software_renderer::Rgb565Pixel;

use crate::{
    backlight::BacklightDevice,
    ft6x36_adapter::Ft6x36TouchAdapter,
    framework::Framework,
    mk_static,
    sdcard_spi::create_sdcard_spi_device_dma,
    slint_ext::McuWindow,
    touch::Touch,
    ui_loop::UiRenderBackend,
};

// For collecting stats on rendering time split
static mut GRAPHICS_TOTAL: u64 = 0;
static mut TOTAL_LINES: u64 = 0;
static mut TOTAL_PIXELS: u64 = 0;

// ===============================================================================================================
// == WT32 Display Renderer Backend ===============================================================================
// ===============================================================================================================

pub struct WT32RenderBackend<DM>
where
    DM: esp_hal::DriverMode,
{
    pub buffer_provider: DrawBuffer<'static, DM>,
}

impl<DM> UiRenderBackend for WT32RenderBackend<DM>
where
    DM: esp_hal::DriverMode,
{
    fn render(&mut self, renderer: &slint::platform::software_renderer::SoftwareRenderer) -> bool {
        let start_graphics_time = embassy_time::Instant::now();

        // For single line rendering (2/2)
        renderer.render_by_line(&mut self.buffer_provider);

        let graphics_time = start_graphics_time.elapsed();
        unsafe {
            GRAPHICS_TOTAL += graphics_time.as_micros();
        }
        true
    }
}

// ===============================================================================================================
// == WT32 Backlight Control ======================================================================================
// ===============================================================================================================

pub struct WT32Backlight {
    channel0: esp_hal::ledc::channel::Channel<'static, LowSpeed>,
    timer: &'static esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::LowSpeed>,
}

impl WT32Backlight {
    pub fn new(
        channel0: esp_hal::ledc::channel::Channel<'static, LowSpeed>,
        timer: &'static esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::LowSpeed>,
    ) -> Self {
        Self { channel0, timer }
    }
}

impl BacklightDevice for WT32Backlight {
    type Error = ();

    fn set_percent(&mut self, percent: u8) -> Result<(), Self::Error> {
        self.channel0
            .configure(esp_hal::ledc::channel::config::Config {
                timer: self.timer,
                duty_pct: percent,
                drive_mode: esp_hal::gpio::DriveMode::PushPull,
            })
            .map_err(|_| ())
    }
}

// ===============================================================================================================
// == Slint Esp Backend Implementation for drawing and timer, specific to this device ============================
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
        let now = esp_hal::time::Instant::now();
        let duration = now.duration_since_epoch();
        core::time::Duration::from_micros(duration.as_micros())
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

// ===============================================================================================================
// == WT32 Rendering Stats =======================================================================================
// ===============================================================================================================

#[embassy_executor::task]
async fn stats_task() {
    loop {
        unsafe {
            dbg!(GRAPHICS_TOTAL, TOTAL_LINES, TOTAL_PIXELS);
        }
        Timer::after_secs(5).await;
    }
}

// ===============================================================================================================
// == WT32 Display Peripherals ===================================================================================
// ===============================================================================================================

#[allow(non_snake_case)]
pub struct WT32SC01PlusDisplayPeripherals<CHLCD, P>
where
    CHLCD: esp_hal::dma::TxChannelFor<LCD_CAM<'static>> + 'static,
    P: esp_hal::i2c::master::Instance + 'static,
{
    pub GPIO47: esp_hal::peripherals::GPIO47<'static>,
    pub GPIO0: esp_hal::peripherals::GPIO0<'static>,
    pub GPIO45: esp_hal::peripherals::GPIO45<'static>,
    pub GPIO4: esp_hal::peripherals::GPIO4<'static>,
    pub LCD_CAM: LCD_CAM<'static>,
    pub GPIO9: esp_hal::peripherals::GPIO9<'static>,
    pub GPIO46: esp_hal::peripherals::GPIO46<'static>,
    pub GPIO3: esp_hal::peripherals::GPIO3<'static>,
    pub GPIO8: esp_hal::peripherals::GPIO8<'static>,
    pub GPIO18: esp_hal::peripherals::GPIO18<'static>,
    pub GPIO17: esp_hal::peripherals::GPIO17<'static>,
    pub GPIO16: esp_hal::peripherals::GPIO16<'static>,
    pub GPIO15: esp_hal::peripherals::GPIO15<'static>,
    pub LEDC: esp_hal::peripherals::LEDC<'static>,
    pub GPIO5: esp_hal::peripherals::GPIO5<'static>,
    pub GPIO6: esp_hal::peripherals::GPIO6<'static>,
    pub GPIO7: esp_hal::peripherals::GPIO7<'static>,
    pub DMA_CHx: CHLCD,
    pub I2Cx: P,
}

// ===============================================================================================================
// == WT32 SDCard Peripherals ====================================================================================
// ===============================================================================================================

#[allow(non_snake_case)]
pub struct WT32SC01PlusSDCardPeripherals<S, CHSD>
where
    S: esp_hal::spi::master::Instance + 'static,
    CHSD: esp_hal::dma::DmaChannelFor<spi::master::AnySpi<'static>>,
{
    pub GPIO38: esp_hal::peripherals::GPIO38<'static>,
    pub GPIO39: esp_hal::peripherals::GPIO39<'static>,
    pub GPIO40: esp_hal::peripherals::GPIO40<'static>,
    pub GPIO41: esp_hal::peripherals::GPIO41<'static>,
    pub SPIx: S,
    pub DMA_CHx: CHSD,
}

// ===============================================================================================================
// == WT32 Board Abstraction =====================================================================================
// ===============================================================================================================

type InitDone = Signal<CriticalSectionRawMutex, Result<(), String>>;

pub struct WT32SC01Plus {
    init_done: &'static InitDone,
}

impl WT32SC01Plus {
    #[allow(clippy::type_complexity)]
    pub fn new<'a, CHLCD, P, S, CHSD>(
        display_peripherals: WT32SC01PlusDisplayPeripherals<CHLCD, P>,
        sdcard_peripherals: WT32SC01PlusSDCardPeripherals<S, CHSD>,
        display_orientation: mipidsi::options::Orientation,
        framework: Rc<RefCell<Framework>>,
    ) -> (
        Self,
        WT32SC01PlusRunner<CHLCD, P>,
        // No DMA version
        // ExclusiveDevice<Spi<'a, esp_hal::Async>, Output<'a>, NoDelay>,

        // DMA Version
        ExclusiveDevice<
            esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>,
            esp_hal::gpio::Output<'a>,
            embedded_hal_bus::spi::NoDelay,
        >,
    )
    where
        CHLCD: esp_hal::dma::TxChannelFor<LCD_CAM<'static>> + 'static,
        P: esp_hal::i2c::master::Instance + 'static,
        S: esp_hal::spi::master::Instance + 'static,
        CHSD: esp_hal::dma::DmaChannelFor<spi::master::AnySpi<'static>> + 'a + 'static,
    {
        let init_done = mk_static!(InitDone, InitDone::new());
        let runner = WT32SC01PlusRunner {
            peripherals: Some(display_peripherals),
            display_orientation,
            framework,
            init_done,
        };
        let me = Self { init_done };

        // ===============================================================================================================
        // == WT32 SDCard Interface =======================================================================================
        // ===============================================================================================================

        let sd_cs = Output::new(
            sdcard_peripherals.GPIO41,
            Level::High,
            OutputConfig::default(),
        );
        let sd_sclk = sdcard_peripherals.GPIO39;
        let sd_miso = sdcard_peripherals.GPIO38;
        let sd_mosi = sdcard_peripherals.GPIO40;
        let spix = sdcard_peripherals.SPIx;

        // Non DMA version ////////////////////////////////////////

        // let sdcard_spi_device = crate::sdcard_spi::create_sdcard_spi_device_no_dma(
        //     spix,
        //     sd_cs,
        //     sd_sclk,
        //     sd_miso,
        //     sd_mosi,
        //     Rate::from_mhz(2),
        // );

        // DMA version /////////////////////////////////////////////

        let sdcard_spi_device = create_sdcard_spi_device_dma(
            spix,
            sdcard_peripherals.DMA_CHx,
            sd_cs,
            sd_sclk,
            sd_miso,
            sd_mosi,
            Rate::from_mhz(20), // 2 or 25.MHz()?
        );

        ////////////////////////////////////////////////////////////

        (me, runner, sdcard_spi_device)
    }
    pub async fn wait_init_done(&self) -> Result<(), String> {
        self.init_done.wait().await
    }
}

// ===============================================================================================================
// == WT32 Board Runner ==========================================================================================
// ===============================================================================================================

pub struct WT32SC01PlusRunner<C, P>
where
    C: esp_hal::dma::TxChannelFor<LCD_CAM<'static>> + 'static,
    P: esp_hal::i2c::master::Instance + 'static,
{
    peripherals: Option<WT32SC01PlusDisplayPeripherals<C, P>>,
    display_orientation: mipidsi::options::Orientation,
    framework: Rc<RefCell<Framework>>,
    init_done: &'static InitDone,
}

impl<C, P> WT32SC01PlusRunner<C, P>
where
    C: esp_hal::dma::TxChannelFor<LCD_CAM<'static>> + 'static,
    P: esp_hal::i2c::master::Instance + 'static,
{
    pub async fn run(&mut self) {
        let mut peripherals = self.peripherals.take().unwrap();

        // ===============================================================================================================
        // == WT32 Runner - Display Interface ==========================================================================
        // ===============================================================================================================

        let di_wr = Output::new(
            peripherals.GPIO47.reborrow(),
            Level::High,
            OutputConfig::default(),
        );
        let di_dc = Output::new(
            peripherals.GPIO0.reborrow(),
            Level::High,
            OutputConfig::default(),
        );
        let di_bl = peripherals.GPIO45;
        let di_rst = Output::new(peripherals.GPIO4, Level::High, OutputConfig::default());

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

        let lcd_cam = LcdCam::new(peripherals.LCD_CAM);

        let di_wr = peripherals.GPIO47;
        let di_dc = peripherals.GPIO0;

        let i8080_config =
            esp_hal::lcd_cam::lcd::i8080::Config::default().with_frequency(Rate::from_mhz(40));

        let mut i8080 = esp_hal::lcd_cam::lcd::i8080::I8080::new(
            lcd_cam.lcd,
            peripherals.DMA_CHx,
            i8080_config,
        )
        .unwrap()
        .with_dc(di_dc)
        .with_wrx(di_wr)
        .with_data0(peripherals.GPIO9)
        .with_data1(peripherals.GPIO46)
        .with_data2(peripherals.GPIO3)
        .with_data3(peripherals.GPIO8)
        .with_data4(peripherals.GPIO18)
        .with_data5(peripherals.GPIO17)
        .with_data6(peripherals.GPIO16)
        .with_data7(peripherals.GPIO15);

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
                frequency: Rate::from_khz(24),
            })
            .unwrap();
        let channel0 = ledc.channel(esp_hal::ledc::channel::Number::Channel0, di_bl);

        // ===============================================================================================================
        // == WT32 Runner - Touch Interface ============================================================================
        // ===============================================================================================================

        let ti_sda = peripherals.GPIO6; //.into_push_pull_output();
        let ti_scl = peripherals.GPIO5; //.into_push_pull_output();
        let ti_irq = Input::new(peripherals.GPIO7, InputConfig::default().with_pull(Pull::Down)); //.into_push_pull_output();

        // TODO: Check the option of switching to async I2C instead of my own interrupt approach
        // let _ti_i2c = esp_hal::i2c::master::I2c::new(peripherals.I2C0, {
        //     let mut config = esp_hal::i2c::master::Config::default();
        //     config.frequency = 400u32.kHz();
        //     config
        // });

        let ti_i2c = esp_hal::i2c::master::I2c::new(
            peripherals.I2Cx,
            esp_hal::i2c::master::Config::default().with_frequency(Rate::from_khz(400)),
        )
        .unwrap()
        .with_sda(ti_sda)
        .with_scl(ti_scl);

        esp_hal::interrupt::enable(
            esp_hal::peripherals::Interrupt::GPIO,
            esp_hal::interrupt::Priority::Priority3,
        )
        .unwrap();

        // ===============================================================================================================
        // == WT32 Runner - Slint Backend ==============================================================================
        // ===============================================================================================================

        let (width, height, ft6x36orientation) = match self.display_orientation.rotation {
            mipidsi::options::Rotation::Deg0 => (320, 480, ft6x36::Orientation::Portrait), // ?? orientation not tested
            mipidsi::options::Rotation::Deg180 => (320, 480, ft6x36::Orientation::InvertedPortrait), // ?? orientation not tested
            mipidsi::options::Rotation::Deg90 => (480, 320, ft6x36::Orientation::Landscape),
            mipidsi::options::Rotation::Deg270 => {
                (480, 320, ft6x36::Orientation::InvertedLandscape)
            }
        };

        let size = slint::PhysicalSize::new(width, height);
        let window =
            McuWindow::new(slint::platform::software_renderer::RepaintBufferType::ReusedBuffer);
        window.set_size(size);
        slint::platform::set_platform(Box::new(EspBackend {
            window: window.clone(),
        }))
        .expect("backend already initialized");

        let mut touch_inner = ft6x36::Ft6x36::new(
            ti_i2c,
            ft6x36::Dimension((height - 1) as u16, (width - 1) as u16),
        );
        touch_inner.set_orientation(ft6x36orientation);
        if touch_inner.init().is_err() {
            panic!(
                "Failed to initialize touch. Did you flash the correct device? (WT32-SC01 Plus)"
            );
        }

        let touch_adapter = Ft6x36TouchAdapter::new(touch_inner, ti_irq);
        let touch = Touch::new(touch_adapter);

        let render_backend = WT32RenderBackend { buffer_provider };
        let mut backlight = WT32Backlight::new(channel0, lstimer0);

        // Turn on display backlight
        backlight
            .set_percent(100)
            .expect("Failed to set display backlight to 100%");

        self.init_done.signal(Ok(()));

        crate::ui_loop::event_loop(touch, window, render_backend, backlight, self.framework.clone())
            .await;
    }
}

// ===============================================================================================================
// == WT32 Fast Display Bus ======================================================================================
// ===============================================================================================================
// WT32-SC01 Fast Display Bus instead of slow display_interface_parallel_gpio bus
// Not really needed since we use DMA now, so this is used only for setup, but may be useful for fast gpio in the future, so using this implementation

#[derive(Default)]
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
