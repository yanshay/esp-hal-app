use alloc::{
    string::{FromUtf8Error, String, ToString},
    vec::Vec,
};
use embedded_hal_async::spi::SpiDevice;
use embedded_sdmmc::asynchronous::{
    sdcard::AcquireOpts, BlockDevice, RawFile, RawVolume, SdCard, VolumeManager,
};

use snafu::{prelude::*, IntoError};

use crate::utils::DebugWrap;

#[derive(Snafu, Debug)]
pub enum Error<E>
where
    E: core::fmt::Debug + 'static,
{
    #[snafu(display("Failed to open volume"))]
    OpenVolume {
        #[snafu(source(from(embedded_sdmmc::asynchronous::Error<E>, DebugWrap)))]
        source: DebugWrap<embedded_sdmmc::asynchronous::Error<E>>,
    },
    #[snafu(display("Failed to open file/dir \'{full_path}\' part \'{part}\' : {source}"))]
    Open {
        full_path: String,
        part: String,
        #[snafu(source(from(embedded_sdmmc::asynchronous::Error<E>, DebugWrap)))]
        source: DebugWrap<embedded_sdmmc::asynchronous::Error<E>>,
    },
    #[snafu(display("Failed to create directory \'{full_path}\' part \'{part}\' : {source}"))]
    MakeDir {
        full_path: String,
        part: String,
        #[snafu(source(from(embedded_sdmmc::asynchronous::Error<E>, DebugWrap)))]
        source: DebugWrap<embedded_sdmmc::asynchronous::Error<E>>,
    },
    #[snafu(display("Failed to change directory \'{full_path}\' part \'{part}\' : {source}"))]
    ChangeDir {
        full_path: String,
        part: String,
        #[snafu(source(from(embedded_sdmmc::asynchronous::Error<E>, DebugWrap)))]
        source: DebugWrap<embedded_sdmmc::asynchronous::Error<E>>,
    },
    #[snafu(display("Failed to close file/dir \'{full_path}\' part \'{part}\' : {source}"))]
    Close {
        full_path: String,
        part: String,
        #[snafu(source(from(embedded_sdmmc::asynchronous::Error<E>, DebugWrap)))]
        source: DebugWrap<embedded_sdmmc::asynchronous::Error<E>>,
    },
    #[snafu(display("Failed to read file \'{full_path}\' : {source}"))]
    ReadFile {
        full_path: String,
        #[snafu(source(from(embedded_sdmmc::asynchronous::Error<E>, DebugWrap)))]
        source: DebugWrap<embedded_sdmmc::asynchronous::Error<E>>,
    },
    #[snafu(display("Failed to write file \'{full_path}\' : {source}"))]
    WriteFile {
        full_path: String,
        #[snafu(source(from(embedded_sdmmc::asynchronous::Error<E>, DebugWrap)))]
        source: DebugWrap<embedded_sdmmc::asynchronous::Error<E>>,
    },
    #[snafu(display("Failed to seek file \'{full_path}\' offset {offset}: {source}"))]
    SeekFile {
        full_path: String,
        offset: u32,
        #[snafu(source(from(embedded_sdmmc::asynchronous::Error<E>, DebugWrap)))]
        source: DebugWrap<embedded_sdmmc::asynchronous::Error<E>>,
    },
    #[snafu(display("Failed to UTF8 decode file \'{full_path}\' : {source}"))]
    DecodeUTF8 {
        full_path: String,
        source: FromUtf8Error,
    },
}

pub struct Clock;

impl embedded_sdmmc::asynchronous::TimeSource for Clock {
    fn get_timestamp(&self) -> embedded_sdmmc::asynchronous::Timestamp {
        embedded_sdmmc::asynchronous::Timestamp {
            year_since_1970: 30_u8,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

pub struct SDCardStore<SPI: SpiDevice, const MAX_DIRS: usize, const MAX_FILES: usize> {
    volume_mgr: VolumeManager<SdCard<SPI, embassy_time::Delay>, Clock, MAX_DIRS, MAX_FILES, 1>,
    raw_volume: Option<RawVolume>,
    pub card_installed: bool,
}

const CREATE_MODES: [embedded_sdmmc::asynchronous::Mode; 4] = [
    embedded_sdmmc::asynchronous::Mode::ReadWriteCreateOrTruncate,
    embedded_sdmmc::asynchronous::Mode::ReadWriteCreate,
    embedded_sdmmc::asynchronous::Mode::ReadWriteCreateOrAppend,
    embedded_sdmmc::asynchronous::Mode::ReadWriteAppend,
];

pub type SDCardStoreErrorSource = Error<embedded_sdmmc::asynchronous::SdCardError>; // used for use with Snafu as the error source type
type SdCardError<SPI> = <SdCard<SPI, embassy_time::Delay> as BlockDevice>::Error;
type SDCardStoreError<SPI> = Error<SdCardError<SPI>>;

impl<SPI: SpiDevice, const MAX_DIRS: usize, const MAX_FILES: usize>
    SDCardStore<SPI, MAX_DIRS, MAX_FILES>
{
    pub async fn new(spi: SPI) -> Self {
        let sdmmc = SdCard::new_with_options(
            spi,
            embassy_time::Delay,
            AcquireOpts {
                acquire_retries: 50,
                ..Default::default()
            },
        );
        let volume_mgr = VolumeManager::<
            embedded_sdmmc::asynchronous::SdCard<SPI, embassy_time::Delay>,
            Clock,
            MAX_DIRS,
            MAX_FILES,
            1,
        >::new_with_limits(sdmmc, Clock {}, 5000);

        let mut volume = None;
        for _ in 0..5 {
            if let Ok(volume0) = volume_mgr
                .open_volume(embedded_sdmmc::asynchronous::VolumeIdx(0))
                .await
            {
                volume = Some(volume0.to_raw_volume());
                break;
            } else {
                volume = None;
            };
        }

        Self {
            card_installed: volume.is_some(),
            volume_mgr,
            raw_volume: volume,
        }
    }

    pub async fn open_volume(&mut self) -> Result<(), SDCardStoreError<SPI>> {
        let raw_volume = self.take_volume().await?;
        self.return_volume(raw_volume);
        Ok(())
    }

    async fn take_volume(&mut self) -> Result<RawVolume, SDCardStoreError<SPI>> {
        if let Some(raw_volume) = self.raw_volume.take() {
            self.card_installed = true;
            return Ok(raw_volume);
        }
        let raw_volume = self
            .volume_mgr
            .open_volume(embedded_sdmmc::asynchronous::VolumeIdx(0))
            .await
            .context(OpenVolumeSnafu)?
            .to_raw_volume();
        Ok(raw_volume)
    }

    fn return_volume(&mut self, raw_volume: RawVolume) {
        self.raw_volume = Some(raw_volume);
    }

    async fn open_file(
        &mut self,
        path: &str,
        mode: embedded_sdmmc::asynchronous::Mode,
    ) -> Result<RawFile, SDCardStoreError<SPI>> {
        let volume0 = self.take_volume().await?.to_volume(&self.volume_mgr);

        let res: Result<RawFile, SDCardStoreError<SPI>> = async {
            let mut dir = volume0.open_root_dir().context(OpenSnafu {
                full_path: path.to_string(),
                part: "/",
            })?;
            let mut last_path_part = "";

            let res: Result<RawFile, SDCardStoreError<SPI>> = async {
                let path_parts: Vec<&str> = path.split(['/', '\\']).collect();
                for (path_idx, path_part) in path_parts.iter().enumerate() {
                    last_path_part = path_part;
                    if path_idx == path_parts.len() - 1 {
                        break;
                    };
                    if path_part.is_empty() {
                        continue;
                    };
                    let res = dir.change_dir(*path_part).await;
                    if let Err(e) = res {
                        if CREATE_MODES.contains(&mode) {
                            dir.make_dir_in_dir(*path_part)
                                .await
                                .context(MakeDirSnafu {
                                    full_path: path.to_string(),
                                    part: path_part.to_string(),
                                })?;
                            dir.change_dir(*path_part).await.context(OpenSnafu {
                                full_path: path.to_string(),
                                part: path_part.to_string(),
                            })?;
                        } else {
                            return Err(ChangeDirSnafu {
                                full_path: path.to_string(),
                                part: path_part.to_string(),
                            }
                            .into_error(e));
                        }
                    }
                }
                let file = dir
                    .open_file_in_dir(path_parts[path_parts.len() - 1], mode)
                    .await
                    .context(OpenSnafu {
                        full_path: path.to_string(),
                        part: path_parts[path_parts.len() - 1].to_string(),
                    })?;
                let raw_file = file.to_raw_file();
                Ok(raw_file)
            }
            .await;

            dir.close().context(CloseSnafu {
                full_path: path.to_string(),
                part: last_path_part,
            })?;

            res
        }
        .await;

        self.return_volume(volume0.to_raw_volume());

        res
    }

    pub async fn inner_read_file_bytes(
        &mut self,
        path: &str,
        mode: embedded_sdmmc::asynchronous::Mode,
    ) -> Result<Vec<u8>, SDCardStoreError<SPI>> {
        let file = self.open_file(path, mode).await?;
        let file = file.to_file(&self.volume_mgr);

        let file_length = file.length();
        let mut buffer = alloc::vec![0u8; file_length as usize];

        let res: Result<Vec<u8>, SDCardStoreError<SPI>> = async {
            let _num_read = file.read(&mut buffer).await.context(ReadFileSnafu {
                full_path: path.to_string(),
            })?;

            Ok(buffer)
        }
        .await;
        file.close().await.context(CloseSnafu {
            full_path: path.to_string(),
            part: "".to_string(),
        })?;

        res
    }

    pub async fn read_file_bytes(&mut self, path: &str) -> Result<Vec<u8>, SDCardStoreError<SPI>> {
        self.inner_read_file_bytes(path, embedded_sdmmc::asynchronous::Mode::ReadOnly)
            .await
    }

    pub async fn append_bytes(
        &mut self,
        path: &str,
        bytes: &[u8],
    ) -> Result<u32, SDCardStoreError<SPI>> {
        let file = self
            .open_file(
                path,
                embedded_sdmmc::asynchronous::Mode::ReadWriteCreateOrAppend,
            )
            .await?;
        let file = file.to_file(&self.volume_mgr);

        let write_offset = file.length();

        let res: Result<u32, SDCardStoreError<SPI>> = async {
            file.write(bytes)
                .await
                .context(WriteFileSnafu { full_path: path })?;
            // .map_err(|e| format_err!("Seek error in file {path} - {e:?}"))?;
            file.flush()
                .await
                .context(WriteFileSnafu { full_path: path })?;
            Ok(write_offset)
        }
        .await;

        // async finally block
        file.close().await.context(CloseSnafu {
            full_path: path,
            part: "",
        })?;

        res
    }

    pub async fn append_text(
        &mut self,
        path: &str,
        text: &str,
    ) -> Result<u32, SDCardStoreError<SPI>> {
        self.append_bytes(path, text.as_bytes()).await
    }

    pub async fn create_write_file_bytes(
        &mut self,
        path: &str,
        bytes: &[u8],
    ) -> Result<(), SDCardStoreError<SPI>> {
        let file = self
            .open_file(
                path,
                embedded_sdmmc::asynchronous::Mode::ReadWriteCreateOrTruncate,
            )
            .await?;

        let file = file.to_file(&self.volume_mgr);

        let res: Result<(), SDCardStoreError<SPI>> = async {
            file.write(bytes)
                .await
                .context(WriteFileSnafu { full_path: path })?;
            file.flush()
                .await
                .context(WriteFileSnafu { full_path: path })?;
            Ok(())
        }
        .await;

        // async finally block
        file.close().await.context(CloseSnafu {
            full_path: path,
            part: "",
        })?;

        res
    }
    pub async fn create_write_file_str(
        &mut self,
        path: &str,
        text: &str,
    ) -> Result<(), SDCardStoreError<SPI>> {
        self.create_write_file_bytes(path, text.as_bytes()).await
    }

    pub async fn write_file_bytes(
        &mut self,
        path: &str,
        offset: u32,
        bytes: &[u8],
        only_if_new: bool,
    ) -> Result<(), SDCardStoreError<SPI>> {
        // TODO:    Not sure this is correct, think of the API, if we want this to create a new file
        let file_open_res = self
            .open_file(path, embedded_sdmmc::asynchronous::Mode::ReadWriteAppend)
            .await;
        let file = match file_open_res {
            Ok(file) => {
                if only_if_new {
                    return Ok(());
                }
                file
            }
            Err(_) => {
                self.open_file(
                    path,
                    embedded_sdmmc::asynchronous::Mode::ReadWriteCreateOrAppend,
                )
                .await?
            }
        };

        let file = file.to_file(&self.volume_mgr);

        let res: Result<(), SDCardStoreError<SPI>> = async {
            file.seek_from_start(offset).context(SeekFileSnafu {
                full_path: path,
                offset,
            })?;
            file.write(bytes)
                .await
                .context(WriteFileSnafu { full_path: path })?;
            file.flush()
                .await
                .context(WriteFileSnafu { full_path: path })?;
            Ok(())
        }
        .await;

        // async finally block
        file.close().await.context(CloseSnafu {
            full_path: path,
            part: "",
        })?;

        res
    }
    #[allow(dead_code)]
    pub async fn write_file_str(
        &mut self,
        path: &str,
        offset: u32,
        text: &str,
        only_if_new: bool,
    ) -> Result<(), SDCardStoreError<SPI>> {
        self.write_file_bytes(path, offset, text.as_bytes(), only_if_new).await
    }

    pub async fn read_file_str(&mut self, path: &str) -> Result<String, SDCardStoreError<SPI>> {
        let file_bin = self.read_file_bytes(path).await?;
        let file_str = String::from_utf8(file_bin).context(DecodeUTF8Snafu { full_path: path })?;

        Ok(file_str)
    }

    pub async fn read_create_bytes(
        &mut self,
        path: &str,
    ) -> Result<Vec<u8>, SDCardStoreError<SPI>> {
        let res = self.read_file_bytes(path).await;
        match res {
            Ok(v) => Ok(v),
            Err(_) => {
                self.inner_read_file_bytes(
                    path,
                    embedded_sdmmc::asynchronous::Mode::ReadWriteCreateOrAppend,
                )
                .await
            }
        }
    }

    pub async fn read_create_str(&mut self, path: &str) -> Result<String, SDCardStoreError<SPI>> {
        let v = self.read_create_bytes(path).await?;
        let s = String::from_utf8(v).context(DecodeUTF8Snafu { full_path: path })?;
        Ok(s)
    }

    pub async fn create_file(&mut self, path: &str) -> Result<(), SDCardStoreError<SPI>> {
        let file = self
            .open_file(
                path,
                embedded_sdmmc::asynchronous::Mode::ReadWriteCreateOrTruncate,
            )
            .await?;
        let file = file.to_file(&self.volume_mgr);
        file.close().await.context(CloseSnafu {
            full_path: path,
            part: "",
        })
    }
}
