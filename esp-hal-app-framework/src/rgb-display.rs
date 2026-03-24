use core::cell::RefCell;

use critical_section::Mutex;
use esp_hal::Blocking;
use esp_hal::dma::{
    self, AnyGdmaChannel, BurstConfig, DmaChannelConvert, DmaDescriptor, DmaEligible, DmaTxBuffer,
    ExternalBurstConfig, InternalBurstConfig, Mem2Mem, Preparation, SimpleMem2Mem,
    SimpleMem2MemTransfer, TransferDirection,
};
use esp_hal::handler;
use esp_hal::interrupt::{self, Priority};
use esp_hal::lcd_cam::lcd::dpi::{Dpi, DpiTransfer};
use esp_hal::peripherals::{DMA, Interrupt};
use esp_hal::ram;
use esp_hal::system::Cpu;
use esp_hal::time::Instant;
use static_cell::StaticCell;

pub enum FrameMode {
    SingleBuffer,
    DoubleBuffering,
}

impl Copy for FrameMode {}
impl Clone for FrameMode {
    fn clone(&self) -> Self {
        *self
    }
}

pub enum FlushPolicy {
    Enabled,
    Disabled,
}

impl Copy for FlushPolicy {}
impl Clone for FlushPolicy {
    fn clone(&self) -> Self {
        *self
    }
}

pub enum RefillPolicy {
    NonBlocking,
    WaitOnMiss,
}

impl Copy for RefillPolicy {}
impl Clone for RefillPolicy {
    fn clone(&self) -> Self {
        *self
    }
}

#[derive(Clone, Copy)]
pub struct RGBDisplayConfig {
    pub width: usize,
    pub height: usize,
    pub bytes_per_pixel: usize,
    pub rows_per_window: usize,
    /// Pixel clock in Hz.
    ///
    /// This should match the value passed to `dpi::Config::with_frequency(...)`.
    pub pixel_clock_hz: u32,
    /// Total pixels per line, including blanking.
    ///
    /// This should match `dpi::FrameTiming::horizontal_total_width`.
    pub horizontal_total_width: u32,
    /// Total lines per frame, including blanking.
    ///
    /// This should match `dpi::FrameTiming::vertical_total_height`.
    pub vertical_total_height: u32,
    pub burst: BurstConfig,
    pub flush: FlushPolicy,
    pub refill_policy: RefillPolicy,
    pub frame_mode: FrameMode,
}

impl RGBDisplayConfig {
    pub const fn frame_bytes(&self) -> usize {
        self.width * self.height * self.bytes_per_pixel
    }

    pub const fn window_bytes(&self) -> usize {
        self.width * self.bytes_per_pixel * self.rows_per_window
    }

    pub const fn windows_len(&self) -> usize {
        self.frame_bytes() / self.window_bytes()
    }

    pub const fn required_bounce_bytes(&self) -> usize {
        self.window_bytes() * 2
    }

    pub const fn required_bounce_out_desc_count(&self) -> usize {
        let per_window = dma::descriptor_count(
            self.window_bytes(),
            self.burst.max_compatible_chunk_size(),
            false,
        );
        per_window * self.windows_len()
    }

    pub const fn required_m2m_desc_count(&self) -> usize {
        dma::descriptor_count(
            self.window_bytes(),
            self.burst.max_compatible_chunk_size(),
            false,
        )
    }

    pub const fn wait_on_miss_timeout_us(&self) -> u64 {
        let reuse_rows = (self.rows_per_window * 2) as u64;
        let frame_time_us = self.frame_time_us() as u64;
        let panel_rows = self.height as u64;
        (frame_time_us * reuse_rows).div_ceil(panel_rows)
    }

    pub const fn frame_time_us(&self) -> u32 {
        ((1_000_000u64 * self.horizontal_total_width as u64 * self.vertical_total_height as u64)
            / self.pixel_clock_hz as u64) as u32
    }
}

pub struct RGBDisplayStorage<'a> {
    pub bounce: &'a mut [u8],
    pub bounce_out_desc: &'a mut [DmaDescriptor],
    pub m2m_src_desc: &'a mut [DmaDescriptor],
    pub m2m_dst_desc: &'a mut [DmaDescriptor],
    #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
    pub precomputed_m2m_src_ptrs: &'a mut [*mut u8],
    #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
    pub precomputed_m2m_dst_ptrs: &'a mut [*mut u8],
}

const WORST_BURST: BurstConfig = BurstConfig {
    internal_memory: InternalBurstConfig::Enabled,
    external_memory: ExternalBurstConfig::Size64,
};

pub const fn display_bounce_bytes(
    width: usize,
    bytes_per_pixel: usize,
    rows_per_window: usize,
) -> usize {
    width * bytes_per_pixel * rows_per_window * 2
}

pub const fn display_bounce_out_desc_count(
    width: usize,
    height: usize,
    bytes_per_pixel: usize,
    rows_per_window: usize,
) -> usize {
    let window_bytes = width * bytes_per_pixel * rows_per_window;
    let frame_bytes = width * height * bytes_per_pixel;
    let windows_len = frame_bytes / window_bytes;
    let per_window =
        dma::descriptor_count(window_bytes, WORST_BURST.max_compatible_chunk_size(), false);
    per_window * windows_len
}

pub const fn display_m2m_desc_count(
    width: usize,
    bytes_per_pixel: usize,
    rows_per_window: usize,
) -> usize {
    let window_bytes = width * bytes_per_pixel * rows_per_window;
    dma::descriptor_count(window_bytes, WORST_BURST.max_compatible_chunk_size(), false)
}

pub const fn display_precomputed_src_ptr_count(
    width: usize,
    height: usize,
    bytes_per_pixel: usize,
    rows_per_window: usize,
) -> usize {
    let window_bytes = width * bytes_per_pixel * rows_per_window;
    let frame_bytes = width * height * bytes_per_pixel;
    let windows_len = frame_bytes / window_bytes;
    2 * windows_len * display_m2m_desc_count(width, bytes_per_pixel, rows_per_window)
}

pub const fn display_precomputed_dst_ptr_count(
    width: usize,
    bytes_per_pixel: usize,
    rows_per_window: usize,
) -> usize {
    2 * display_m2m_desc_count(width, bytes_per_pixel, rows_per_window)
}

#[repr(align(64))]
pub struct AlignedBytes<const N: usize>(pub [u8; N]);

pub struct RGBDisplayDmaStorage<
    const BOUNCE_BYTES: usize,
    const BOUNCE_OUT_DESC_COUNT: usize,
    const M2M_DESC_COUNT: usize,
    const PRECOMPUTED_SRC_PTR_COUNT: usize,
    const PRECOMPUTED_DST_PTR_COUNT: usize,
> {
    bounce: AlignedBytes<BOUNCE_BYTES>,
    bounce_out_desc: [DmaDescriptor; BOUNCE_OUT_DESC_COUNT],
    m2m_src_desc: [DmaDescriptor; M2M_DESC_COUNT],
    m2m_dst_desc: [DmaDescriptor; M2M_DESC_COUNT],
    #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
    precomputed_m2m_src_ptrs: [*mut u8; PRECOMPUTED_SRC_PTR_COUNT],
    #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
    precomputed_m2m_dst_ptrs: [*mut u8; PRECOMPUTED_DST_PTR_COUNT],
}

impl<
    const BOUNCE_BYTES: usize,
    const BOUNCE_OUT_DESC_COUNT: usize,
    const M2M_DESC_COUNT: usize,
    const PRECOMPUTED_SRC_PTR_COUNT: usize,
    const PRECOMPUTED_DST_PTR_COUNT: usize,
>
    RGBDisplayDmaStorage<
        BOUNCE_BYTES,
        BOUNCE_OUT_DESC_COUNT,
        M2M_DESC_COUNT,
        PRECOMPUTED_SRC_PTR_COUNT,
        PRECOMPUTED_DST_PTR_COUNT,
    >
{
    pub const fn new() -> Self {
        Self {
            bounce: AlignedBytes([0; BOUNCE_BYTES]),
            bounce_out_desc: [DmaDescriptor::EMPTY; BOUNCE_OUT_DESC_COUNT],
            m2m_src_desc: [DmaDescriptor::EMPTY; M2M_DESC_COUNT],
            m2m_dst_desc: [DmaDescriptor::EMPTY; M2M_DESC_COUNT],
            #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
            precomputed_m2m_src_ptrs: [core::ptr::null_mut(); PRECOMPUTED_SRC_PTR_COUNT],
            #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
            precomputed_m2m_dst_ptrs: [core::ptr::null_mut(); PRECOMPUTED_DST_PTR_COUNT],
        }
    }

    pub fn as_storage_mut(&'static mut self) -> RGBDisplayStorage<'static> {
        RGBDisplayStorage {
            bounce: &mut self.bounce.0,
            bounce_out_desc: &mut self.bounce_out_desc,
            m2m_src_desc: &mut self.m2m_src_desc,
            m2m_dst_desc: &mut self.m2m_dst_desc,
            #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
            precomputed_m2m_src_ptrs: &mut self.precomputed_m2m_src_ptrs,
            #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
            precomputed_m2m_dst_ptrs: &mut self.precomputed_m2m_dst_ptrs,
        }
    }
}

pub struct RGBDisplayResources<Dma, Spi> {
    pub dpi: Dpi<'static, Blocking>,
    pub dma: Dma,
    pub spi: Spi,
    pub frames: &'static mut [&'static mut [u8]],
}

#[derive(Debug)]
pub enum RGBDisplayError {
    InvalidConfig,
    BufferSizeMismatch,
    AlignmentMismatch,
    DescriptorCountMismatch,
}

#[cfg(feature = "rgb-stats")]
#[derive(Clone, Copy, Default)]
pub struct RGBDisplayStats {
    /// Count of OUT EOF interrupts that fired while a mem2mem transfer handle
    /// was still marked in-flight.
    ///
    /// With the dual-ISR model this is an overlap/jitter indicator, not a
    /// strict "hardware transfer late" signal.
    pub out_eof_while_inflight_count: u32,
    /// Count of times a newer phase target for the same bounce half replaced an
    /// older pending target before it could be consumed.
    pub pending_same_half_overwrite_count: u32,
    /// Count of mem2mem transfers started (PSRAM frame window -> bounce half).
    pub m2m_copy_start_count: u32,
    /// Count of transmitted windows where the bounce half did not contain the
    /// expected window index at OUT EOF time.
    ///
    /// This is the direct "real miss" signal for PSRAM->RAM refill deadlines.
    pub stale_window_tx_count: u32,
    /// Count of WaitOnMiss miss rendezvous cases where OUT EOF observed the miss
    /// before the inbound done hint arrived.
    pub wait_on_miss_out_first_count: u32,
    /// Count of WaitOnMiss miss rendezvous cases where the optional inbound done
    /// hint was observed before OUT EOF detected the miss.
    #[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
    pub wait_on_miss_done_hint_first_count: u32,
    /// Total time spent blocked inside WaitOnMiss completion waits, in us.
    pub wait_on_miss_wait_total_us: u64,
    /// Number of WaitOnMiss completion waits that were timed.
    pub wait_on_miss_wait_count: u32,
    /// Number of WaitOnMiss waits that exceeded the timeout threshold.
    pub wait_on_miss_timeout_count: u32,
    /// Total time spent inside the OUT EOF ISR, in us.
    pub out_isr_total_us: u64,
    /// Number of OUT EOF ISR invocations timed.
    pub out_isr_count: u32,
    /// Total time spent inside the IN DMA ISR, in us.
    pub in_isr_total_us: u64,
    /// Number of IN DMA ISR invocations timed.
    pub in_isr_count: u32,
}

#[cfg(not(feature = "rgb-stats"))]
#[derive(Clone, Copy, Default)]
pub struct RGBDisplayStats;

#[cfg(feature = "rgb-stats")]
macro_rules! stats_inc {
    ($state:expr, $field:ident) => {
        $state.stats.$field = $state.stats.$field.wrapping_add(1);
    };
}

#[cfg(not(feature = "rgb-stats"))]
macro_rules! stats_inc {
    ($state:expr, $field:ident) => {
        let _ = $state;
    };
}

type M2mTransfer = SimpleMem2MemTransfer<'static, 'static, Blocking>;

struct DisplayState {
    frame_len: usize,
    frame_ptrs: [*mut u8; 2],
    frame_count: u8,
    active_frame_idx: u8,
    pending_frame_idx: Option<u8>,
    writer_frame_idx: Option<u8>,

    window_bytes: usize,
    windows_len: usize,
    out_desc_start: *const DmaDescriptor,
    out_desc_count: usize,
    out_desc_per_window: usize,

    flush: bool,
    mem2mem: *mut SimpleMem2Mem<'static, Blocking>,
    m2m_src_desc: *mut DmaDescriptor,
    m2m_dst_desc: *mut DmaDescriptor,
    m2m_desc_count: usize,
    bounce_base: *mut u8,

    #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
    precomputed_src_ptrs: *const *mut u8,
    #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
    precomputed_dst_ptrs: *const *mut u8,

    in_flight_copy: Option<M2mTransfer>,
    in_flight_target_buffer: usize,
    in_flight_window_index: usize,
    loaded_window_for_buffer: [usize; 2],
    window_index_next: usize,
    work_pending: bool,
    refill_wait_on_miss: bool,
    wait_on_miss_timeout_us: u64,
    wait_on_miss_out_seen: bool,
    #[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
    wait_on_miss_in_done_seen: bool,
    #[cfg(feature = "rgb-stats")]
    stats: RGBDisplayStats,
}

unsafe impl Send for DisplayState {}

static DISPLAY_STATE: Mutex<RefCell<Option<DisplayState>>> = Mutex::new(RefCell::new(None));
static MEM2MEM_ENGINE: StaticCell<SimpleMem2Mem<'static, Blocking>> = StaticCell::new();

struct BounceTxBuf {
    preparation: Preparation,
}

unsafe impl DmaTxBuffer for BounceTxBuf {
    type View = Self;
    type Final = Self;

    fn prepare(&mut self) -> Preparation {
        Preparation {
            start: self.preparation.start,
            direction: self.preparation.direction,
            accesses_psram: self.preparation.accesses_psram,
            burst_transfer: self.preparation.burst_transfer,
            check_owner: self.preparation.check_owner,
            auto_write_back: self.preparation.auto_write_back,
        }
    }

    fn into_view(self) -> Self::View {
        self
    }

    fn from_view(view: Self::View) -> Self::Final {
        view
    }
}

pub struct RGBDisplayDriver {
    dpi: Option<Dpi<'static, Blocking>>,
    tx_buf: Option<BounceTxBuf>,
    transfer: Option<DpiTransfer<'static, BounceTxBuf, Blocking>>,
}

pub struct FrameWriteGuard<'a> {
    driver: &'a mut RGBDisplayDriver,
    frame_idx: u8,
    presented: bool,
}

impl<'a> FrameWriteGuard<'a> {
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.driver.frame_slice_mut(self.frame_idx)
    }

    pub fn present(mut self) -> Result<(), RGBDisplayError> {
        self.driver.present_frame_idx(self.frame_idx)?;
        self.presented = true;
        Ok(())
    }
}

impl Drop for FrameWriteGuard<'_> {
    fn drop(&mut self) {
        if !self.presented {
            self.driver.release_writer(self.frame_idx);
        }
    }
}

impl RGBDisplayDriver {
    pub const fn required_bounce_bytes(cfg: &RGBDisplayConfig) -> usize {
        cfg.required_bounce_bytes()
    }

    pub const fn required_bounce_out_desc_count(cfg: &RGBDisplayConfig) -> usize {
        cfg.required_bounce_out_desc_count()
    }

    pub const fn required_m2m_desc_count(cfg: &RGBDisplayConfig) -> usize {
        cfg.required_m2m_desc_count()
    }

    pub fn new<Dma, Spi>(
        cfg: RGBDisplayConfig,
        storage: RGBDisplayStorage<'static>,
        resources: RGBDisplayResources<Dma, Spi>,
    ) -> Result<Self, RGBDisplayError>
    where
        Dma: DmaChannelConvert<AnyGdmaChannel<'static>>,
        Spi: DmaEligible,
    {
        validate_config(&cfg)?;
        validate_storage(&cfg, &storage)?;
        let (frame_ptrs, frame_count) = validate_frames(&cfg, resources.frames)?;

        let window_bytes = cfg.window_bytes();
        let windows_len = cfg.windows_len();
        let out_desc_per_window =
            dma::descriptor_count(window_bytes, cfg.burst.max_compatible_chunk_size(), false);
        let out_desc_count = out_desc_per_window * windows_len;

        let m2m_src_ptr = storage.m2m_src_desc.as_mut_ptr();
        let m2m_dst_ptr = storage.m2m_dst_desc.as_mut_ptr();
        let m2m_desc_count = storage.m2m_src_desc.len();

        let m2m_src_desc: &'static mut [DmaDescriptor] =
            unsafe { core::slice::from_raw_parts_mut(m2m_src_ptr, m2m_desc_count) };
        let m2m_dst_desc: &'static mut [DmaDescriptor] =
            unsafe { core::slice::from_raw_parts_mut(m2m_dst_ptr, m2m_desc_count) };

        init_linear_descriptors(
            m2m_src_desc,
            window_bytes,
            true,
            cfg.burst.max_compatible_chunk_size(),
        );
        init_linear_descriptors(
            m2m_dst_desc,
            window_bytes,
            false,
            cfg.burst.max_compatible_chunk_size(),
        );

        let mem2mem = Mem2Mem::new(resources.dma, resources.spi)
            .with_descriptors(m2m_dst_desc, m2m_src_desc, cfg.burst)
            .unwrap();
        let mem2mem = MEM2MEM_ENGINE.init(mem2mem);

        prefill_bounce_buffer_with_dma(
            mem2mem,
            frame_ptrs[0],
            0,
            storage.bounce.as_mut_ptr(),
            window_bytes,
            cfg,
            unsafe { core::slice::from_raw_parts_mut(m2m_src_ptr, m2m_desc_count) },
            unsafe { core::slice::from_raw_parts_mut(m2m_dst_ptr, m2m_desc_count) },
        );
        prefill_bounce_buffer_with_dma(
            mem2mem,
            frame_ptrs[0],
            1,
            unsafe { storage.bounce.as_mut_ptr().add(window_bytes) },
            window_bytes,
            cfg,
            unsafe { core::slice::from_raw_parts_mut(m2m_src_ptr, m2m_desc_count) },
            unsafe { core::slice::from_raw_parts_mut(m2m_dst_ptr, m2m_desc_count) },
        );

        #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
        fill_precomputed_m2m_pointer_tables(
            frame_ptrs,
            frame_count,
            windows_len,
            window_bytes,
            storage.bounce.as_mut_ptr(),
            unsafe { core::slice::from_raw_parts(m2m_src_ptr, m2m_desc_count) },
            storage.precomputed_m2m_src_ptrs,
            storage.precomputed_m2m_dst_ptrs,
        );

        let out_desc_start = prepare_outbound_descriptors(
            storage.bounce_out_desc,
            storage.bounce.as_mut_ptr(),
            window_bytes,
            windows_len,
            cfg.burst.max_compatible_chunk_size(),
        );

        critical_section::with(|cs| {
            *DISPLAY_STATE.borrow_ref_mut(cs) = Some(DisplayState {
                frame_len: cfg.frame_bytes(),
                frame_ptrs,
                frame_count,
                active_frame_idx: 0,
                pending_frame_idx: None,
                writer_frame_idx: None,
                window_bytes,
                windows_len,
                out_desc_start,
                out_desc_count,
                out_desc_per_window,
                flush: matches!(cfg.flush, FlushPolicy::Enabled),
                mem2mem,
                m2m_src_desc: m2m_src_ptr,
                m2m_dst_desc: m2m_dst_ptr,
                m2m_desc_count,
                bounce_base: storage.bounce.as_mut_ptr(),
                #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
                precomputed_src_ptrs: storage.precomputed_m2m_src_ptrs.as_ptr(),
                #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
                precomputed_dst_ptrs: storage.precomputed_m2m_dst_ptrs.as_ptr(),
                in_flight_copy: None,
                in_flight_target_buffer: 0,
                in_flight_window_index: 0,
                loaded_window_for_buffer: [0, 1],
                window_index_next: 2 % windows_len,
                work_pending: false,
                refill_wait_on_miss: matches!(cfg.refill_policy, RefillPolicy::WaitOnMiss),
                wait_on_miss_timeout_us: cfg.wait_on_miss_timeout_us(),
                wait_on_miss_out_seen: false,
                #[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
                wait_on_miss_in_done_seen: false,
                #[cfg(feature = "rgb-stats")]
                stats: RGBDisplayStats::default(),
            });
        });

        bind_out_eof_interrupt();
        bind_in_interrupts();

        Ok(Self {
            dpi: Some(resources.dpi),
            tx_buf: Some(BounceTxBuf {
                preparation: Preparation {
                    start: out_desc_start as *mut DmaDescriptor,
                    direction: TransferDirection::Out,
                    accesses_psram: false,
                    burst_transfer: cfg.burst,
                    check_owner: Some(false),
                    auto_write_back: false,
                },
            }),
            transfer: None,
        })
    }

    pub fn start(&mut self) -> Result<(), RGBDisplayError> {
        if self.transfer.is_some() {
            return Ok(());
        }

        let dpi = self.dpi.take().ok_or(RGBDisplayError::InvalidConfig)?;
        let tx_buf = self.tx_buf.take().ok_or(RGBDisplayError::InvalidConfig)?;

        critical_section::with(|cs| {
            if let Some(state) = DISPLAY_STATE.borrow_ref_mut(cs).as_mut() {
                reset_runtime_state(state);
            }
        });

        clear_out_eof_interrupt();
        clear_in_interrupts();
        enable_out_eof_interrupt();
        enable_in_interrupts();

        match dpi.send(true, tx_buf) {
            Ok(transfer) => {
                self.transfer = Some(transfer);
                Ok(())
            }
            Err((_err, dpi, tx_buf)) => {
                self.dpi = Some(dpi);
                self.tx_buf = Some(tx_buf);
                Err(RGBDisplayError::InvalidConfig)
            }
        }
    }

    pub fn stop(&mut self) {
        disable_in_interrupts();
        disable_out_eof_interrupt();
        clear_in_interrupts();
        clear_out_eof_interrupt();

        if let Some(transfer) = self.transfer.take() {
            let (dpi, tx_buf) = transfer.stop();
            self.dpi = Some(dpi);
            self.tx_buf = Some(tx_buf);
        }

        critical_section::with(|cs| {
            if let Some(state) = DISPLAY_STATE.borrow_ref_mut(cs).as_mut() {
                reset_runtime_state(state);
            }
        });
    }

    pub fn is_running(&self) -> bool {
        self.transfer.is_some()
    }

    pub fn take_stats() -> RGBDisplayStats {
        #[cfg(not(feature = "rgb-stats"))]
        {
            RGBDisplayStats
        }

        #[cfg(feature = "rgb-stats")]
        critical_section::with(|cs| {
            let mut guard = DISPLAY_STATE.borrow_ref_mut(cs);
            let Some(state) = guard.as_mut() else {
                return RGBDisplayStats::default();
            };
            core::mem::take(&mut state.stats)
        })
    }

    pub fn acquire_writable_frame(&mut self) -> Option<FrameWriteGuard<'_>> {
        let frame_idx = critical_section::with(|cs| {
            let mut guard = DISPLAY_STATE.borrow_ref_mut(cs);
            let state = guard.as_mut()?;

            if let Some(idx) = state.writer_frame_idx {
                return Some(idx);
            }

            let idx = if state.frame_count == 1 {
                state.active_frame_idx
            } else {
                state.active_frame_idx ^ 1
            };

            if state.frame_count == 2 && state.pending_frame_idx == Some(idx) {
                return None;
            }

            state.writer_frame_idx = Some(idx);
            Some(idx)
        })?;

        Some(FrameWriteGuard {
            driver: self,
            frame_idx,
            presented: false,
        })
    }

    fn frame_slice_mut(&mut self, frame_idx: u8) -> &mut [u8] {
        let (ptr, len) = critical_section::with(|cs| {
            let mut guard = DISPLAY_STATE.borrow_ref_mut(cs);
            let state = guard.as_mut().expect("display state missing");
            let idx = frame_idx as usize;
            assert!(idx < state.frame_count as usize);
            (state.frame_ptrs[idx], state.frame_len)
        });

        unsafe { core::slice::from_raw_parts_mut(ptr, len) }
    }

    fn release_writer(&mut self, frame_idx: u8) {
        critical_section::with(|cs| {
            if let Some(state) = DISPLAY_STATE.borrow_ref_mut(cs).as_mut() {
                if state.writer_frame_idx == Some(frame_idx) {
                    state.writer_frame_idx = None;
                }
            }
        });
    }

    fn present_frame_idx(&mut self, frame_idx: u8) -> Result<(), RGBDisplayError> {
        let (ptr, len, flush, single) = critical_section::with(|cs| {
            let mut guard = DISPLAY_STATE.borrow_ref_mut(cs);
            let state = guard.as_mut().ok_or(RGBDisplayError::InvalidConfig)?;

            if state.writer_frame_idx != Some(frame_idx) {
                return Err(RGBDisplayError::InvalidConfig);
            }

            let idx = frame_idx as usize;
            if idx >= state.frame_count as usize {
                return Err(RGBDisplayError::InvalidConfig);
            }

            state.writer_frame_idx = None;

            let single = state.frame_count == 1;
            if !single {
                state.pending_frame_idx = Some(frame_idx);
            }

            Ok((state.frame_ptrs[idx], state.frame_len, state.flush, single))
        })?;

        if flush {
            unsafe { cache_writeback_addr(ptr as u32, len as u32) };
        }

        let _ = single;
        Ok(())
    }
}

fn reset_runtime_state(state: &mut DisplayState) {
    state.window_index_next = 2 % state.windows_len;
    state.work_pending = false;
    state.in_flight_target_buffer = 0;
    state.in_flight_window_index = 0;
    state.loaded_window_for_buffer = [0, 1];
    clear_wait_on_miss_flags(state);
    #[cfg(feature = "rgb-stats")]
    {
        state.stats = RGBDisplayStats::default();
    }
    state.pending_frame_idx = None;
    state.writer_frame_idx = None;
    if let Some(transfer) = state.in_flight_copy.take() {
        let _ = transfer.wait();
    }
}

fn validate_config(cfg: &RGBDisplayConfig) -> Result<(), RGBDisplayError> {
    if cfg.width == 0
        || cfg.height == 0
        || cfg.bytes_per_pixel == 0
        || cfg.rows_per_window == 0
        || cfg.pixel_clock_hz == 0
        || cfg.horizontal_total_width == 0
        || cfg.vertical_total_height == 0
        || cfg.wait_on_miss_timeout_us() == 0
        || !cfg.height.is_multiple_of(cfg.rows_per_window)
    {
        return Err(RGBDisplayError::InvalidConfig);
    }
    Ok(())
}

fn validate_frames(
    cfg: &RGBDisplayConfig,
    frames: &mut [&'static mut [u8]],
) -> Result<([*mut u8; 2], u8), RGBDisplayError> {
    let required = match cfg.frame_mode {
        FrameMode::SingleBuffer => 1,
        FrameMode::DoubleBuffering => 2,
    };

    if frames.len() != required {
        return Err(RGBDisplayError::BufferSizeMismatch);
    }

    let frame_len = cfg.frame_bytes();
    let mut ptrs = [core::ptr::null_mut(); 2];

    for (i, frame) in frames.iter_mut().enumerate() {
        if frame.len() != frame_len {
            return Err(RGBDisplayError::BufferSizeMismatch);
        }
        ptrs[i] = frame.as_mut_ptr();
    }

    Ok((ptrs, required as u8))
}

fn validate_storage(
    cfg: &RGBDisplayConfig,
    storage: &RGBDisplayStorage<'_>,
) -> Result<(), RGBDisplayError> {
    if storage.bounce.len() != cfg.required_bounce_bytes() {
        return Err(RGBDisplayError::BufferSizeMismatch);
    }
    if storage.bounce_out_desc.len() < cfg.required_bounce_out_desc_count() {
        return Err(RGBDisplayError::DescriptorCountMismatch);
    }
    if storage.m2m_src_desc.len() < cfg.required_m2m_desc_count()
        || storage.m2m_dst_desc.len() < cfg.required_m2m_desc_count()
    {
        return Err(RGBDisplayError::DescriptorCountMismatch);
    }

    let alignment = cfg.burst.min_compatible_alignment();
    if !(storage.bounce.as_ptr() as usize).is_multiple_of(alignment) {
        return Err(RGBDisplayError::AlignmentMismatch);
    }
    Ok(())
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(link_section = ".rwtext")]
unsafe fn cache_writeback_addr(addr: u32, size: u32) {
    unsafe extern "C" {
        fn rom_Cache_WriteBack_Addr(addr: u32, size: u32);
        fn Cache_Suspend_DCache_Autoload() -> u32;
        fn Cache_Resume_DCache_Autoload(value: u32);
    }

    unsafe {
        let autoload = Cache_Suspend_DCache_Autoload();
        rom_Cache_WriteBack_Addr(addr, size);
        Cache_Resume_DCache_Autoload(autoload);
    }
}

#[ram]
fn retarget_linear_descriptors(
    descriptors: &mut [DmaDescriptor],
    buffer_ptr: *mut u8,
    is_tx: bool,
) {
    let mut offset = 0usize;
    let descriptors_len = descriptors.len();
    for (i, desc) in descriptors.iter_mut().enumerate() {
        let size = desc.size();
        desc.buffer = unsafe { buffer_ptr.add(offset) };
        if is_tx {
            desc.set_length(size);
            desc.reset_for_tx(i + 1 == descriptors_len);
        } else {
            desc.reset_for_rx();
        }
        offset += size;
    }
}

fn init_linear_descriptors(
    descriptors: &mut [DmaDescriptor],
    len: usize,
    is_tx: bool,
    max_chunk_size: usize,
) {
    let mut next = core::ptr::null_mut();
    for desc in descriptors.iter_mut().rev() {
        desc.next = next;
        next = desc;
    }

    let mut remaining = len;
    let descriptors_len = descriptors.len();
    for (i, desc) in descriptors.iter_mut().enumerate() {
        let chunk = core::cmp::min(max_chunk_size, remaining);
        desc.set_size(chunk);
        if is_tx {
            desc.set_length(chunk);
            desc.reset_for_tx(i + 1 == descriptors_len);
        } else {
            desc.reset_for_rx();
        }
        remaining -= chunk;
    }
}

fn prepare_outbound_descriptors(
    descriptors: &mut [DmaDescriptor],
    bounce_base: *mut u8,
    window_bytes: usize,
    windows_len: usize,
    max_chunk_size: usize,
) -> *const DmaDescriptor {
    let first = descriptors.as_mut_ptr();
    let mut next = first;
    for desc in descriptors.iter_mut().rev() {
        desc.next = next;
        next = desc;
    }

    let out_desc_per_window = dma::descriptor_count(window_bytes, max_chunk_size, false);

    for window_index in 0..windows_len {
        let bounce_offset = (window_index % 2) * window_bytes;
        let bounce_ptr = unsafe { bounce_base.add(bounce_offset) };

        for i in 0..out_desc_per_window {
            let segment_offset = i * max_chunk_size;
            let remaining = window_bytes - segment_offset;
            let chunk = core::cmp::min(max_chunk_size, remaining);

            let desc_index = window_index * out_desc_per_window + i;
            let desc = &mut descriptors[desc_index];
            desc.buffer = unsafe { bounce_ptr.add(segment_offset) };
            desc.set_size(chunk);
            desc.set_length(chunk);
            desc.reset_for_tx(i + 1 == out_desc_per_window);
        }
    }

    first
}

#[ram]
fn start_copy_to_bounce_buffer(
    state: &mut DisplayState,
    target_buffer_index: usize,
    window_index: usize,
) {
    clear_wait_on_miss_flags(state);

    let frame_idx = state.active_frame_idx as usize;
    assert!(frame_idx < state.frame_count as usize);
    let frame_ptr = state.frame_ptrs[frame_idx];

    let frame_offset = (window_index % state.windows_len) * state.window_bytes;
    let src_ptr = unsafe { frame_ptr.add(frame_offset) };
    let dst_ptr = unsafe {
        state
            .bounce_base
            .add(target_buffer_index * state.window_bytes)
    };

    let src = unsafe { core::slice::from_raw_parts(src_ptr as *const u8, state.window_bytes) };
    let dst = unsafe { core::slice::from_raw_parts_mut(dst_ptr, state.window_bytes) };

    let src_desc =
        unsafe { core::slice::from_raw_parts_mut(state.m2m_src_desc, state.m2m_desc_count) };
    let dst_desc =
        unsafe { core::slice::from_raw_parts_mut(state.m2m_dst_desc, state.m2m_desc_count) };

    #[cfg(feature = "rgb-precomputed-m2m-descriptors-off")]
    {
        retarget_linear_descriptors(src_desc, src_ptr, true);
        retarget_linear_descriptors(dst_desc, dst_ptr, false);
    }

    #[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
    {
        let frame_idx = state.active_frame_idx as usize;
        let window_idx = window_index % state.windows_len;
        let src_table_len = state.frame_count as usize * state.windows_len * state.m2m_desc_count;
        let src_table =
            unsafe { core::slice::from_raw_parts(state.precomputed_src_ptrs, src_table_len) };
        let src_base = (frame_idx * state.windows_len + window_idx) * state.m2m_desc_count;

        let dst_table_len = 2 * state.m2m_desc_count;
        let dst_table =
            unsafe { core::slice::from_raw_parts(state.precomputed_dst_ptrs, dst_table_len) };
        let dst_base = target_buffer_index * state.m2m_desc_count;

        for i in 0..state.m2m_desc_count {
            let srcd = &mut src_desc[i];
            srcd.buffer = src_table[src_base + i];
            srcd.set_length(srcd.size());
            srcd.reset_for_tx(i + 1 == state.m2m_desc_count);

            let dstd = &mut dst_desc[i];
            dstd.buffer = dst_table[dst_base + i];
            dstd.reset_for_rx();
        }
    }

    let mem2mem: &'static mut SimpleMem2Mem<'static, Blocking> = unsafe { &mut *state.mem2mem };
    state.in_flight_copy = Some(mem2mem.start_transfer(dst, src).unwrap());
}

fn prefill_bounce_buffer_with_dma(
    mem2mem: &mut SimpleMem2Mem<'static, Blocking>,
    frame_ptr: *mut u8,
    window_index: usize,
    bounce_ptr: *mut u8,
    window_bytes: usize,
    cfg: RGBDisplayConfig,
    m2m_src_desc: &mut [DmaDescriptor],
    m2m_dst_desc: &mut [DmaDescriptor],
) {
    let src_ptr = unsafe { frame_ptr.add(window_index * window_bytes) };

    if matches!(cfg.flush, FlushPolicy::Enabled) {
        unsafe { cache_writeback_addr(src_ptr as u32, window_bytes as u32) }
    }

    retarget_linear_descriptors(m2m_src_desc, src_ptr, true);
    retarget_linear_descriptors(m2m_dst_desc, bounce_ptr, false);

    let src = unsafe { core::slice::from_raw_parts(src_ptr as *const u8, window_bytes) };
    let dst = unsafe { core::slice::from_raw_parts_mut(bounce_ptr, window_bytes) };
    mem2mem.start_transfer(dst, src).unwrap().wait().unwrap();
}

#[cfg(not(feature = "rgb-precomputed-m2m-descriptors-off"))]
fn fill_precomputed_m2m_pointer_tables(
    frame_ptrs: [*mut u8; 2],
    frame_count: u8,
    windows_len: usize,
    window_bytes: usize,
    bounce_base: *mut u8,
    m2m_src_desc: &[DmaDescriptor],
    precomputed_src_ptrs: &mut [*mut u8],
    precomputed_dst_ptrs: &mut [*mut u8],
) {
    let desc_count = m2m_src_desc.len();

    let required_src = frame_count as usize * windows_len * desc_count;
    let required_dst = 2 * desc_count;
    assert!(precomputed_src_ptrs.len() >= required_src);
    assert!(precomputed_dst_ptrs.len() >= required_dst);

    for frame_idx in 0..frame_count as usize {
        let frame_ptr = frame_ptrs[frame_idx];
        for window_idx in 0..windows_len {
            let window_base = unsafe { frame_ptr.add(window_idx * window_bytes) };
            let base = (frame_idx * windows_len + window_idx) * desc_count;
            let mut offset = 0usize;
            for (i, desc) in m2m_src_desc.iter().enumerate() {
                precomputed_src_ptrs[base + i] = unsafe { window_base.add(offset) };
                offset += desc.size();
            }
        }
    }

    for target_buffer in 0..2usize {
        let bounce_window_base = unsafe { bounce_base.add(target_buffer * window_bytes) };
        let base = target_buffer * desc_count;
        let mut offset = 0usize;
        for (i, desc) in m2m_src_desc.iter().enumerate() {
            precomputed_dst_ptrs[base + i] = unsafe { bounce_window_base.add(offset) };
            offset += desc.size();
        }
    }
}

#[ram]
fn try_start_pending_copy(state: &mut DisplayState) {
    if state.in_flight_copy.is_some() || !state.work_pending {
        return;
    }

    state.work_pending = false;
    let window_index = state.window_index_next;
    let target_buffer = window_index % 2;
    state.in_flight_target_buffer = target_buffer;
    state.in_flight_window_index = window_index;
    stats_inc!(state, m2m_copy_start_count);
    start_copy_to_bounce_buffer(state, target_buffer, window_index);
}

#[inline(always)]
fn clear_wait_on_miss_flags(state: &mut DisplayState) {
    state.wait_on_miss_out_seen = false;
    #[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
    {
        state.wait_on_miss_in_done_seen = false;
    }
}

#[inline(always)]
fn complete_in_flight_copy(state: &mut DisplayState) {
    if let Some(transfer) = state.in_flight_copy.take() {
        let t0 = Instant::now();

        let transfer = transfer;
        let mut timed_out = false;
        while !transfer.is_done() {
            if t0.elapsed().as_micros() >= state.wait_on_miss_timeout_us {
                timed_out = true;
                break;
            }
        }

        #[cfg(feature = "rgb-stats")]
        {
            let elapsed_us = t0.elapsed().as_micros();
            state.stats.wait_on_miss_wait_total_us = state
                .stats
                .wait_on_miss_wait_total_us
                .wrapping_add(elapsed_us);
            state.stats.wait_on_miss_wait_count =
                state.stats.wait_on_miss_wait_count.wrapping_add(1);
            if timed_out {
                state.stats.wait_on_miss_timeout_count =
                    state.stats.wait_on_miss_timeout_count.wrapping_add(1);
            }
        }

        if !timed_out {
            state.loaded_window_for_buffer[state.in_flight_target_buffer] =
                state.in_flight_window_index;
        }
    }
    clear_wait_on_miss_flags(state);
}

#[inline(always)]
fn on_wait_on_miss_out_event(state: &mut DisplayState) {
    #[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
    {
        if state.wait_on_miss_in_done_seen {
            stats_inc!(state, wait_on_miss_done_hint_first_count);
            complete_in_flight_copy(state);
        } else {
            state.wait_on_miss_out_seen = true;
        }
    }

    #[cfg(not(feature = "rgb-wait-on-miss-done-hint-on"))]
    {
        complete_in_flight_copy(state);
    }
}

#[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
#[inline(always)]
fn on_wait_on_miss_in_done_event(state: &mut DisplayState) {
    if state.wait_on_miss_out_seen {
        stats_inc!(state, wait_on_miss_out_first_count);
        complete_in_flight_copy(state);
    } else {
        state.wait_on_miss_in_done_seen = true;
    }
}

#[cfg(feature = "rgb-stats")]
#[inline(always)]
fn record_out_isr_time(elapsed_us: u64) {
    critical_section::with(|cs| {
        let mut guard = DISPLAY_STATE.borrow_ref_mut(cs);
        if let Some(state) = guard.as_mut() {
            state.stats.out_isr_total_us = state.stats.out_isr_total_us.wrapping_add(elapsed_us);
            stats_inc!(state, out_isr_count);
        }
    });
}

#[cfg(feature = "rgb-stats")]
#[inline(always)]
fn record_in_isr_time(elapsed_us: u64) {
    critical_section::with(|cs| {
        let mut guard = DISPLAY_STATE.borrow_ref_mut(cs);
        if let Some(state) = guard.as_mut() {
            state.stats.in_isr_total_us = state.stats.in_isr_total_us.wrapping_add(elapsed_us);
            stats_inc!(state, in_isr_count);
        }
    });
}

fn enable_out_eof_interrupt() {
    DMA::regs()
        .ch(2)
        .out_int()
        .ena()
        .modify(|_, w| w.out_eof().bit(true));
    interrupt::enable(
        Interrupt::DMA_OUT_CH2,
        dma_outbound_interrupt_handler.priority(),
    )
    .unwrap();
}

fn enable_in_interrupts() {
    #[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
    DMA::regs()
        .ch(0)
        .in_int()
        .ena()
        .modify(|_, w| w.in_suc_eof().bit(true).in_done().bit(true));
    #[cfg(not(feature = "rgb-wait-on-miss-done-hint-on"))]
    DMA::regs()
        .ch(0)
        .in_int()
        .ena()
        .modify(|_, w| w.in_suc_eof().bit(true).in_done().bit(false));
    interrupt::enable(
        Interrupt::DMA_IN_CH0,
        dma_inbound_interrupt_handler.priority(),
    )
    .unwrap();
}

fn disable_out_eof_interrupt() {
    DMA::regs()
        .ch(2)
        .out_int()
        .ena()
        .modify(|_, w| w.out_eof().bit(false));
    interrupt::disable(Cpu::current(), Interrupt::DMA_OUT_CH2);
}

fn disable_in_interrupts() {
    DMA::regs()
        .ch(0)
        .in_int()
        .ena()
        .modify(|_, w| w.in_suc_eof().bit(false).in_done().bit(false));
    interrupt::disable(Cpu::current(), Interrupt::DMA_IN_CH0);
}

fn clear_out_eof_interrupt() {
    DMA::regs()
        .ch(2)
        .out_int()
        .clr()
        .write(|w| w.out_eof().bit(true));
}

fn clear_in_interrupts() {
    #[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
    DMA::regs()
        .ch(0)
        .in_int()
        .clr()
        .write(|w| w.in_suc_eof().bit(true).in_done().bit(true));
    #[cfg(not(feature = "rgb-wait-on-miss-done-hint-on"))]
    DMA::regs()
        .ch(0)
        .in_int()
        .clr()
        .write(|w| w.in_suc_eof().bit(true).in_done().bit(false));
}

fn bind_out_eof_interrupt() {
    unsafe {
        interrupt::bind_interrupt(
            Interrupt::DMA_OUT_CH2,
            dma_outbound_interrupt_handler.handler(),
        );
    }
    clear_out_eof_interrupt();
}

fn bind_in_interrupts() {
    unsafe {
        interrupt::bind_interrupt(
            Interrupt::DMA_IN_CH0,
            dma_inbound_interrupt_handler.handler(),
        );
    }
    clear_in_interrupts();
}

#[handler(priority = Priority::Priority2)]
#[ram]
fn dma_outbound_interrupt_handler() {
    #[cfg(feature = "rgb-stats")]
    let isr_start = Instant::now();

    (|| {
        let out_int = DMA::regs().ch(2).out_int();
        if !out_int.st().read().out_eof().bit_is_set() {
            return;
        }
        out_int.clr().write(|w| w.out_eof().bit(true));

        let eof_desc_addr = DMA::regs()
            .ch(2)
            .out_eof_des_addr()
            .read()
            .out_eof_des_addr()
            .bits() as *const DmaDescriptor;

        critical_section::with(|cs| {
            let mut guard = DISPLAY_STATE.borrow_ref_mut(cs);
            let Some(state) = guard.as_mut() else {
                return;
            };

            let desc_offset = unsafe { eof_desc_addr.offset_from(state.out_desc_start) };
            if desc_offset < 0 {
                return;
            }
            let desc_offset = desc_offset as usize;
            if desc_offset >= state.out_desc_count {
                return;
            }

            let window_sent_index = desc_offset / state.out_desc_per_window;
            let sent_buffer = window_sent_index % 2;
            let refill_window_index = (window_sent_index + 2) % state.windows_len;

            if state.loaded_window_for_buffer[sent_buffer] != window_sent_index {
                stats_inc!(state, stale_window_tx_count);
            }

            if refill_window_index == 0 {
                if let Some(pending) = state.pending_frame_idx.take() {
                    state.active_frame_idx = pending;
                }
            }

            if state.work_pending && state.window_index_next % 2 == sent_buffer {
                stats_inc!(state, pending_same_half_overwrite_count);
            }

            state.window_index_next = refill_window_index;
            state.work_pending = true;

            if state.in_flight_copy.is_some() {
                stats_inc!(state, out_eof_while_inflight_count);

                if state.refill_wait_on_miss {
                    on_wait_on_miss_out_event(state);
                } else {
                    return;
                }
            }

            try_start_pending_copy(state);
        });
    })();

    #[cfg(feature = "rgb-stats")]
    record_out_isr_time(isr_start.elapsed().as_micros());
}

#[handler(priority = Priority::Priority3)]
#[ram]
fn dma_inbound_interrupt_handler() {
    #[cfg(feature = "rgb-stats")]
    let isr_start = Instant::now();

    (|| {
        let in_int = DMA::regs().ch(0).in_int();
        let status = in_int.st().read();
        #[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
        let in_done = status.in_done().bit_is_set();
        let in_suc_eof = status.in_suc_eof().bit_is_set();

        #[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
        if !in_done && !in_suc_eof {
            return;
        }
        #[cfg(not(feature = "rgb-wait-on-miss-done-hint-on"))]
        if !in_suc_eof {
            return;
        }

        #[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
        in_int
            .clr()
            .write(|w| w.in_done().bit(in_done).in_suc_eof().bit(in_suc_eof));
        #[cfg(not(feature = "rgb-wait-on-miss-done-hint-on"))]
        in_int
            .clr()
            .write(|w| w.in_done().bit(false).in_suc_eof().bit(in_suc_eof));

        critical_section::with(|cs| {
            let mut guard = DISPLAY_STATE.borrow_ref_mut(cs);
            let Some(state) = guard.as_mut() else {
                return;
            };

            if state.in_flight_copy.is_none() {
                clear_wait_on_miss_flags(state);
                return;
            }

            #[cfg(feature = "rgb-wait-on-miss-done-hint-on")]
            if in_done && state.refill_wait_on_miss {
                on_wait_on_miss_in_done_event(state);
            }

            if state.in_flight_copy.is_some() && in_suc_eof && !state.wait_on_miss_out_seen {
                let _ = state.in_flight_copy.take();
                state.loaded_window_for_buffer[state.in_flight_target_buffer] =
                    state.in_flight_window_index;
                clear_wait_on_miss_flags(state);
            }

            try_start_pending_copy(state);
        });
    })();

    #[cfg(feature = "rgb-stats")]
    record_in_isr_time(isr_start.elapsed().as_micros());
}
