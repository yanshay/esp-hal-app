use alloc::{string::String, vec, vec::Vec};
use anyhow::{format_err, Result};
use embedded_hal::{delay::DelayNs, spi::SpiDevice};
use embedded_sdmmc::sdcard::AcquireOpts;

pub struct Clock;

impl embedded_sdmmc::TimeSource for Clock {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp {
            year_since_1970: 30_u8,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

pub struct SDCard<SPI: SpiDevice, DELAYER: DelayNs> {
    sdmmc: Option<embedded_sdmmc::SdCard<SPI, DELAYER>>,
}

impl<SPI: SpiDevice, DELAYER: DelayNs> SDCard<SPI, DELAYER> {
    pub fn new(spi: SPI, delay: DELAYER) -> Self {
        Self {
            sdmmc: Some(embedded_sdmmc::SdCard::new_with_options(spi, delay, AcquireOpts{
                acquire_retries: 1,
                ..Default::default()
            })),
        }
    }

    pub fn read_file_bin(&mut self, path: &str) -> Result<Vec<u8>> {
        let sdcard = self.sdmmc.take().unwrap();
        let mut volume_mgr = embedded_sdmmc::VolumeManager::new(sdcard, Clock {});
        let mut volume0 = volume_mgr
            .open_volume(embedded_sdmmc::VolumeIdx(0))
            .map_err(|err| format_err!("Can't open SSD Volume0 - {:?}", err))?;

        let mut dir = volume0.open_root_dir().map_err(|_| format_err!("Can't open SSD root directory"))?;

        let path_parts: Vec<&str> = path.split('/').collect();
        for (path_idx, path_part) in path_parts.iter().enumerate() {
            if path_idx == path_parts.len() - 1 {
                break;
            };
            if path_part.is_empty() {
                continue;
            };
            dir.change_dir(*path_part)
                .map_err(|e| format_err!("Can't open folder {path_part} in {path}, error {e:?}"))?;
        }
        let mut file = dir
            .open_file_in_dir(path_parts[path_parts.len() - 1], embedded_sdmmc::Mode::ReadOnly)
            .map_err(|e| format_err!("Can't open file {} in {path}, error: {e:?}", path_parts[path_parts.len() - 1]))?;

        let file_length = file.length(); //.map_err(|_| format_err!("Can't read file length of {path}"))?;
        let mut buffer = vec![0u8; file_length as usize];

        let _num_read = file.read(&mut buffer).map_err(|_| format_err!("Can't read file {path}"))?;

        drop(file);
        drop(dir);
        drop(volume0);
        let (sdcard, _) = volume_mgr.free();
        self.sdmmc = Some(sdcard);

        Ok(buffer)
    }

    pub fn read_file_str(&mut self, path: &str) -> Result<String> {
        let file_bin = self.read_file_bin(path)?;
        let file_str = String::from_utf8(file_bin).map_err(|_| format_err!("File {path} is not a valid UTF8 file"))?;

        Ok(file_str)
    }
}
