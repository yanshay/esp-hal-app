use embedded_hal_bus::spi::ExclusiveDevice;
use esp_hal::{
    dma::DmaTxBuf,
    gpio::Output,
    spi::{self, master::Spi},
    time::Rate,
};

// ===============================================================================================================
// == Shared SDCard SPI Device Setup =============================================================================
// ===============================================================================================================

#[allow(clippy::too_many_arguments)]
pub fn create_sdcard_spi_device_dma<'a, S, CHSD, SCLK, MISO, MOSI>(
    spix: S,
    dma_ch: CHSD,
    sd_cs: Output<'a>,
    sd_sclk: SCLK,
    sd_miso: MISO,
    sd_mosi: MOSI,
    frequency: Rate,
) -> ExclusiveDevice<
    esp_hal::spi::master::SpiDmaBus<'a, esp_hal::Async>,
    esp_hal::gpio::Output<'a>,
    embedded_hal_bus::spi::NoDelay,
>
where
    'a: 'static,
    S: esp_hal::spi::master::Instance + 'static,
    CHSD: esp_hal::dma::DmaChannelFor<spi::master::AnySpi<'static>> + 'a,
    SCLK: esp_hal::gpio::OutputPin + 'static,
    MISO: esp_hal::gpio::InputPin + 'static,
    MOSI: esp_hal::gpio::OutputPin + 'static,
{
    use esp_hal::dma::DmaRxBuf;
    const DMA_BUFFER_SIZE: usize = 1024;

    let (tx_buffer, tx_descriptors, _, _) = esp_hal::dma_buffers!(DMA_BUFFER_SIZE, 0);
    // info!(
    //     "tx: {:p} len {} ({} descriptors)",
    //     tx_buffer.as_ptr(),
    //     tx_buffer.len(),
    //     tx_descriptors.len()
    // );
    let dma_tx_buf = DmaTxBuf::new(tx_descriptors, tx_buffer).unwrap();

    let (rx_buffer, rx_descriptors, _, _) = esp_hal::dma_buffers!(DMA_BUFFER_SIZE, 0);
    // info!(
    //     "RX: {:p} len {} ({} descriptors)",
    //     rx_buffer.as_ptr(),
    //     rx_buffer.len(),
    //     rx_descriptors.len()
    // );
    let dma_rx_buf = DmaRxBuf::new(rx_descriptors, rx_buffer).unwrap();
    // Need to set miso first so that mosi can overwrite the
    // output connection (because we are using the same pin to loop back)
    let spi_bus = Spi::new(
        spix,
        spi::master::Config::default()
            .with_frequency(frequency)
            .with_mode(spi::Mode::_0),
    )
    .unwrap()
    .with_sck(sd_sclk)
    .with_miso(sd_miso)
    .with_mosi(sd_mosi)
    .with_dma(dma_ch)
    .with_buffers(dma_rx_buf, dma_tx_buf)
    .into_async();

    ExclusiveDevice::new_no_delay(spi_bus, sd_cs).unwrap()
}

// Non DMA version ////////////////////////////////////////

// pub fn create_sdcard_spi_device_no_dma<'a, S, SCLK, MISO, MOSI>(
//     spix: S,
//     sd_cs: Output<'a>,
//     sd_sclk: SCLK,
//     sd_miso: MISO,
//     sd_mosi: MOSI,
//     frequency: Rate,
// ) -> ExclusiveDevice<
//     esp_hal::spi::master::Spi<'a, esp_hal::Async>,
//     esp_hal::gpio::Output<'a>,
//     embedded_hal_bus::spi::NoDelay,
// >
// where
//     S: esp_hal::spi::master::Instance + 'static,
// {
//     let spi_bus = Spi::new(
//         spix,
//         spi::master::Config::default()
//             .with_frequency(frequency)
//             .with_mode(spi::Mode::_0),
//     )
//     .unwrap()
//     .with_sck(sd_sclk)
//     .with_miso(sd_miso)
//     .with_mosi(sd_mosi)
//     .into_async();
//
        // let sdcard_spi_device: ExclusiveDevice<
        //     esp_hal::spi::master::Spi<'_, esp_hal::Async>,
        //     esp_hal::gpio::Output<'_>,
        //     embedded_hal_bus::spi::NoDelay,
        // > = ExclusiveDevice::new_no_delay(spi_bus, sd_cs).unwrap();

//     ExclusiveDevice::new_no_delay(spi_bus, sd_cs).unwrap()
// }
