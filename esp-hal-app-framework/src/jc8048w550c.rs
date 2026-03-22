use alloc::{boxed::Box, rc::Rc, string::String};
use core::{cell::RefCell, slice};

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
use embassy_time::Timer;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{
    dma::{AnyGdmaChannel, BurstConfig, DmaChannelConvert, ExternalBurstConfig, InternalBurstConfig},
    gpio::{Level, Output, OutputConfig},
    lcd_cam::{LcdCam, lcd::dpi::{Config as DpiConfig, Dpi, Format, FrameTiming}},
    ledc::{LowSpeed, channel::ChannelIFace, timer::TimerIFace},
    peripherals::LCD_CAM,
    spi,
    time::Rate,
};

use crate::{
    backlight::BacklightDevice,
    framework::Framework,
    gt9x_adapter::{Gt9xAdapter, Gt9xAdapterConfig, Jc8048w550cGt911},
    mk_static,
    rgb_display::{
        FrameMode, FlushPolicy, RGBDisplayConfig, RGBDisplayDriver, RGBDisplayDmaStorage,
        RefillPolicy,
        RGBDisplayResources, display_bounce_bytes, display_bounce_out_desc_count,
        display_m2m_desc_count, display_precomputed_dst_ptr_count, display_precomputed_src_ptr_count,
    },
    sdcard_spi::create_sdcard_spi_device_dma,
    slint_ext::McuWindow,
    touch::Touch,
    ui_loop::UiRenderBackend,
};

const DISP_W: usize = 800;
const DISP_H: usize = 480;
const DISP_BPP: usize = 2;
const DISP_ROWS: usize = 8;
const DISP_FRAME_BYTES: usize = DISP_W * DISP_H * DISP_BPP;

const DISP_BOUNCE_BYTES: usize = display_bounce_bytes(DISP_W, DISP_BPP, DISP_ROWS);
const DISP_BOUNCE_OUT_DESC_COUNT: usize =
    display_bounce_out_desc_count(DISP_W, DISP_H, DISP_BPP, DISP_ROWS);
const DISP_M2M_DESC_COUNT: usize = display_m2m_desc_count(DISP_W, DISP_BPP, DISP_ROWS);
const DISP_PRECOMPUTED_SRC_PTR_COUNT: usize =
    display_precomputed_src_ptr_count(DISP_W, DISP_H, DISP_BPP, DISP_ROWS);
const DISP_PRECOMPUTED_DST_PTR_COUNT: usize =
    display_precomputed_dst_ptr_count(DISP_W, DISP_BPP, DISP_ROWS);

type DisplayDmaStorage = RGBDisplayDmaStorage<
    DISP_BOUNCE_BYTES,
    DISP_BOUNCE_OUT_DESC_COUNT,
    DISP_M2M_DESC_COUNT,
    DISP_PRECOMPUTED_SRC_PTR_COUNT,
    DISP_PRECOMPUTED_DST_PTR_COUNT,
>;

#[cfg(feature = "rgb-stats")]
#[embassy_executor::task]
async fn stats_task() {
    loop {
        let stats = crate::rgb_display::RGBDisplayDriver::take_stats();
        info!(
            "m2m_stats/s out_eof_while_inflight={} pending_same_half_overwrite={} m2m_copy_start={} stale_window_tx={}",
            stats.out_eof_while_inflight_count,
            stats.pending_same_half_overwrite_count,
            stats.m2m_copy_start_count,
            stats.stale_window_tx_count
        );
        Timer::after_secs(1).await;
    }
}

pub struct Jc8048w550cRenderBackend {
    display: RGBDisplayDriver,
}

impl UiRenderBackend for Jc8048w550cRenderBackend {
    fn render(&mut self, renderer: &slint::platform::software_renderer::SoftwareRenderer) {
        if let Some(mut frame_guard) = self.display.acquire_writable_frame() {
            struct FrameLineBuffer<'a> {
                frame_buffer: &'a mut [slint::platform::software_renderer::Rgb565Pixel],
                stride: usize,
            }

            impl<'a> slint::platform::software_renderer::LineBufferProvider for FrameLineBuffer<'a> {
                type TargetPixel = slint::platform::software_renderer::Rgb565Pixel;

                fn process_line(
                    &mut self,
                    line: usize,
                    range: core::ops::Range<usize>,
                    render_fn: impl FnOnce(&mut [Self::TargetPixel]),
                ) {
                    let line_begin = line * self.stride;
                    render_fn(&mut self.frame_buffer[line_begin..][range]);
                }
            }

            let frame = frame_guard.buffer_mut();
            let pixel_count = frame.len() / core::mem::size_of::<slint::platform::software_renderer::Rgb565Pixel>();
            let pixels: &mut [slint::platform::software_renderer::Rgb565Pixel] =
                unsafe { slice::from_raw_parts_mut(frame.as_mut_ptr() as *mut _, pixel_count) };

            renderer.render_by_line(FrameLineBuffer {
                frame_buffer: pixels,
                stride: DISP_W,
            });
            frame_guard
                .present()
                .expect("Failed to present RGB display frame");
        }
    }
}

pub struct Jc8048w550cBacklight {
    channel0: esp_hal::ledc::channel::Channel<'static, LowSpeed>,
    timer: &'static esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::LowSpeed>,
}

impl Jc8048w550cBacklight {
    pub fn new(
        channel0: esp_hal::ledc::channel::Channel<'static, LowSpeed>,
        timer: &'static esp_hal::ledc::timer::Timer<'static, esp_hal::ledc::LowSpeed>,
    ) -> Self {
        Self { channel0, timer }
    }
}

impl BacklightDevice for Jc8048w550cBacklight {
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

#[allow(non_snake_case)]
pub struct Jc8048w550cDisplayPeripherals<CHLCD, CHM2M, SPIM2M, P>
where
    CHLCD: esp_hal::dma::TxChannelFor<LCD_CAM<'static>> + 'static,
    CHM2M: DmaChannelConvert<AnyGdmaChannel<'static>> + 'static,
    SPIM2M: esp_hal::dma::DmaEligible + 'static,
    P: esp_hal::i2c::master::Instance + 'static,
{
    pub LCD_CAM: LCD_CAM<'static>,
    pub DMA_CH_DPI: CHLCD,
    pub DMA_CH_M2M: CHM2M,
    pub SPI_M2M: SPIM2M,
    pub LEDC: esp_hal::peripherals::LEDC<'static>,
    pub I2Cx: P,

    pub GPIO41: esp_hal::peripherals::GPIO41<'static>,
    pub GPIO39: esp_hal::peripherals::GPIO39<'static>,
    pub GPIO40: esp_hal::peripherals::GPIO40<'static>,
    pub GPIO42: esp_hal::peripherals::GPIO42<'static>,
    pub GPIO8: esp_hal::peripherals::GPIO8<'static>,
    pub GPIO3: esp_hal::peripherals::GPIO3<'static>,
    pub GPIO46: esp_hal::peripherals::GPIO46<'static>,
    pub GPIO9: esp_hal::peripherals::GPIO9<'static>,
    pub GPIO1: esp_hal::peripherals::GPIO1<'static>,
    pub GPIO5: esp_hal::peripherals::GPIO5<'static>,
    pub GPIO6: esp_hal::peripherals::GPIO6<'static>,
    pub GPIO7: esp_hal::peripherals::GPIO7<'static>,
    pub GPIO15: esp_hal::peripherals::GPIO15<'static>,
    pub GPIO16: esp_hal::peripherals::GPIO16<'static>,
    pub GPIO4: esp_hal::peripherals::GPIO4<'static>,
    pub GPIO45: esp_hal::peripherals::GPIO45<'static>,
    pub GPIO48: esp_hal::peripherals::GPIO48<'static>,
    pub GPIO47: esp_hal::peripherals::GPIO47<'static>,
    pub GPIO21: esp_hal::peripherals::GPIO21<'static>,
    pub GPIO14: esp_hal::peripherals::GPIO14<'static>,

    pub GPIO2: esp_hal::peripherals::GPIO2<'static>,

    pub GPIO19: esp_hal::peripherals::GPIO19<'static>,
    pub GPIO20: esp_hal::peripherals::GPIO20<'static>,
    pub GPIO38: esp_hal::peripherals::GPIO38<'static>,
}

#[allow(non_snake_case)]
pub struct Jc8048w550cSDCardPeripherals<S, CHSD>
where
    S: esp_hal::spi::master::Instance + 'static,
    CHSD: esp_hal::dma::DmaChannelFor<spi::master::AnySpi<'static>>,
{
    pub GPIO10: esp_hal::peripherals::GPIO10<'static>,
    pub GPIO11: esp_hal::peripherals::GPIO11<'static>,
    pub GPIO12: esp_hal::peripherals::GPIO12<'static>,
    pub GPIO13: esp_hal::peripherals::GPIO13<'static>,
    pub SPIx: S,
    pub DMA_CHx: CHSD,
}

type InitDone = Signal<CriticalSectionRawMutex, Result<(), String>>;

pub struct Jc8048w550c {
    init_done: &'static InitDone,
}

impl Jc8048w550c {
    #[allow(clippy::type_complexity)]
    pub fn new<'a, CHLCD, CHM2M, SPIM2M, P, S, CHSD>(
        display_peripherals: Jc8048w550cDisplayPeripherals<CHLCD, CHM2M, SPIM2M, P>,
        sdcard_peripherals: Jc8048w550cSDCardPeripherals<S, CHSD>,
        frame_buffer: &'static mut [u8],
        touch_config: Gt9xAdapterConfig,
        framework: Rc<RefCell<Framework>>,
    ) -> (
        Self,
        Jc8048w550cRunner<CHLCD, CHM2M, SPIM2M, P>,
        ExclusiveDevice<
            esp_hal::spi::master::SpiDmaBus<'static, esp_hal::Async>,
            esp_hal::gpio::Output<'a>,
            embedded_hal_bus::spi::NoDelay,
        >,
    )
    where
        CHLCD: esp_hal::dma::TxChannelFor<LCD_CAM<'static>> + 'static,
        CHM2M: DmaChannelConvert<AnyGdmaChannel<'static>> + 'static,
        SPIM2M: esp_hal::dma::DmaEligible + 'static,
        P: esp_hal::i2c::master::Instance + 'static,
        S: esp_hal::spi::master::Instance + 'static,
        CHSD: esp_hal::dma::DmaChannelFor<spi::master::AnySpi<'static>> + 'a + 'static,
    {
        let init_done = mk_static!(InitDone, InitDone::new());
        let runner = Jc8048w550cRunner {
            peripherals: Some(display_peripherals),
            frame_buffer: Some(frame_buffer),
            touch_config,
            framework,
            init_done,
        };
        let me = Self { init_done };

        let sd_cs = Output::new(
            sdcard_peripherals.GPIO10,
            Level::High,
            OutputConfig::default(),
        );
        let sd_sclk = sdcard_peripherals.GPIO12;
        let sd_miso = sdcard_peripherals.GPIO13;
        let sd_mosi = sdcard_peripherals.GPIO11;

        let sdcard_spi_device = create_sdcard_spi_device_dma(
            sdcard_peripherals.SPIx,
            sdcard_peripherals.DMA_CHx,
            sd_cs,
            sd_sclk,
            sd_miso,
            sd_mosi,
            Rate::from_mhz(20),
        );

        (me, runner, sdcard_spi_device)
    }

    pub async fn wait_init_done(&self) -> Result<(), String> {
        self.init_done.wait().await
    }
}

pub struct Jc8048w550cRunner<CHLCD, CHM2M, SPIM2M, P>
where
    CHLCD: esp_hal::dma::TxChannelFor<LCD_CAM<'static>> + 'static,
    CHM2M: DmaChannelConvert<AnyGdmaChannel<'static>> + 'static,
    SPIM2M: esp_hal::dma::DmaEligible + 'static,
    P: esp_hal::i2c::master::Instance + 'static,
{
    peripherals: Option<Jc8048w550cDisplayPeripherals<CHLCD, CHM2M, SPIM2M, P>>,
    frame_buffer: Option<&'static mut [u8]>,
    touch_config: Gt9xAdapterConfig,
    framework: Rc<RefCell<Framework>>,
    init_done: &'static InitDone,
}

impl<CHLCD, CHM2M, SPIM2M, P> Jc8048w550cRunner<CHLCD, CHM2M, SPIM2M, P>
where
    CHLCD: esp_hal::dma::TxChannelFor<LCD_CAM<'static>> + 'static,
    CHM2M: DmaChannelConvert<AnyGdmaChannel<'static>> + 'static,
    SPIM2M: esp_hal::dma::DmaEligible + 'static,
    P: esp_hal::i2c::master::Instance + 'static,
{
    pub async fn run(&mut self) {
        let peripherals = self.peripherals.take().expect("Display peripherals missing");
        let frame_buffer = self.frame_buffer.take().expect("Display frame buffer missing");

        assert!(
            frame_buffer.len() == DISP_FRAME_BYTES,
            "Frame buffer length mismatch: expected {} bytes, got {}",
            DISP_FRAME_BYTES,
            frame_buffer.len()
        );

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
        let channel0 = ledc.channel(esp_hal::ledc::channel::Number::Channel0, peripherals.GPIO2);

        let lcd_cam = LcdCam::new(peripherals.LCD_CAM);
        let dpi_cfg = DpiConfig::default()
            .with_clock_mode(esp_hal::lcd_cam::lcd::ClockMode {
                polarity: esp_hal::lcd_cam::lcd::Polarity::IdleLow,
                phase: esp_hal::lcd_cam::lcd::Phase::ShiftHigh,
            })
            .with_frequency(Rate::from_hz(13_800_000))
            .with_format(Format {
                enable_2byte_mode: true,
                ..Default::default()
            })
            .with_timing(FrameTiming {
                horizontal_active_width: DISP_W,
                horizontal_total_width: 808,
                horizontal_blank_front_porch: 8,
                vertical_active_height: DISP_H,
                vertical_total_height: 488,
                vertical_blank_front_porch: 0,
                hsync_width: 4,
                vsync_width: 4,
                hsync_position: 8,
            })
            .with_vsync_idle_level(Level::Low)
            .with_hsync_idle_level(Level::Low)
            .with_de_idle_level(Level::Low)
            .with_disable_black_region(false);

        let dpi: Dpi<'static, esp_hal::Blocking> = Dpi::new(lcd_cam.lcd, peripherals.DMA_CH_DPI, dpi_cfg)
            .unwrap()
            .with_vsync(peripherals.GPIO41)
            .with_hsync(peripherals.GPIO39)
            .with_de(peripherals.GPIO40)
            .with_pclk(peripherals.GPIO42)
            .with_data0(peripherals.GPIO8)
            .with_data1(peripherals.GPIO3)
            .with_data2(peripherals.GPIO46)
            .with_data3(peripherals.GPIO9)
            .with_data4(peripherals.GPIO1)
            .with_data5(peripherals.GPIO5)
            .with_data6(peripherals.GPIO6)
            .with_data7(peripherals.GPIO7)
            .with_data8(peripherals.GPIO15)
            .with_data9(peripherals.GPIO16)
            .with_data10(peripherals.GPIO4)
            .with_data11(peripherals.GPIO45)
            .with_data12(peripherals.GPIO48)
            .with_data13(peripherals.GPIO47)
            .with_data14(peripherals.GPIO21)
            .with_data15(peripherals.GPIO14);

        let cfg = RGBDisplayConfig {
            width: DISP_W,
            height: DISP_H,
            bytes_per_pixel: DISP_BPP,
            rows_per_window: DISP_ROWS,
            burst: BurstConfig {
                internal_memory: InternalBurstConfig::Enabled,
                external_memory: ExternalBurstConfig::Size64,
            },
            flush: FlushPolicy::Enabled,
            refill_policy: RefillPolicy::WaitOnMiss,
            frame_mode: FrameMode::SingleBuffer,
        };

        let display_dma_storage = mk_static!(DisplayDmaStorage, DisplayDmaStorage::new());
        let dma_storage = display_dma_storage.as_storage_mut();
        let frames: &'static mut [&'static mut [u8]] = Box::leak(Box::new([frame_buffer]));

        let display_resources = RGBDisplayResources {
            dpi,
            dma: peripherals.DMA_CH_M2M,
            spi: peripherals.SPI_M2M,
            frames,
        };

        let mut display = RGBDisplayDriver::new(cfg, dma_storage, display_resources)
            .expect("Failed to create RGB display driver");
        display.start().expect("Failed to start RGB display driver");

        #[cfg(feature = "rgb-stats")]
        self.framework.borrow().spawner.spawn(stats_task()).ok();

        let window = McuWindow::new(slint::platform::software_renderer::RepaintBufferType::ReusedBuffer);
        window.set_size(slint::PhysicalSize::new(DISP_W as u32, DISP_H as u32));
        slint::platform::set_platform(Box::new(EspBackend {
            window: window.clone(),
        }))
        .expect("backend already initialized");

        let mut touch_rst = Output::new(peripherals.GPIO38, Level::High, OutputConfig::default());
        touch_rst.set_low();
        Timer::after_millis(10).await;
        touch_rst.set_high();
        Timer::after_millis(100).await;

        let touch_i2c = esp_hal::i2c::master::I2c::new(
            peripherals.I2Cx,
            esp_hal::i2c::master::Config::default(),
        )
        .unwrap()
        .with_sda(peripherals.GPIO19)
        .with_scl(peripherals.GPIO20)
        .into_async();

        let mut touch_buf = [0u8; 64];
        let mut touch_inner: gt9x::Gt9x<Jc8048w550cGt911, _, _, _, _> =
            gt9x::Gt9x::new(touch_i2c, &mut touch_buf);
        touch_inner
            .init()
            .await
            .expect("Failed to initialize GT9x touch controller");

        let touch = Touch::new(Gt9xAdapter::new(touch_inner, self.touch_config));
        let render_backend = Jc8048w550cRenderBackend { display };
        let mut backlight = Jc8048w550cBacklight::new(channel0, lstimer0);

        backlight
            .set_percent(100)
            .expect("Failed to set display backlight to 100%");

        self.init_done.signal(Ok(()));

        crate::ui_loop::event_loop(touch, window, render_backend, backlight, self.framework.clone())
            .await;
    }
}
