use alloc::{string::String, vec::Vec};
use core::ops::Range;
use embedded_storage::ReadStorage;
use embedded_storage_async::nor_flash::MultiwriteNorFlash;
use esp_partition_table::PartitionTable;
use sequential_storage::{cache::NoCache, Error};

pub struct FlashMap<S: MultiwriteNorFlash> {
    nor_flash: S,
    addr_range: Range<u32>,
    max_buf_size: usize,
    buffer: Vec<u8>,
}

// PartitionTable needs ReadStorage, sequencial_read needs NorFlash, so building ReadStorage based on FlashMap using its async NorFlash
impl<S: MultiwriteNorFlash> ReadStorage for FlashMap<S> {
    type Error = S::Error;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        embassy_futures::block_on(self.nor_flash.read(offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.addr_range.end as usize
    }
}

impl<S: MultiwriteNorFlash> FlashMap<S> {
    #[allow(dead_code)]
    pub async fn new_in_addr_range(
        nor_flash: S,
        addr_range: Range<u32>,
        max_buf_size: usize, // max_buf_size should be the the sum of max key len + max value len + 2 (bytes)
        name: &str,
    ) -> Result<Self, Error<S::Error>> {
        let mut flash_map = Self {
            addr_range,
            nor_flash,
            max_buf_size,
            buffer: Vec::new(),
        };
        flash_map.init_flash_map(name).await?;
        // const MAGIC_KEY: &str = "__map_name__";
        // let magic = flash_map.fetch(String::from(MAGIC_KEY)).await?;
        //
        // if magic.is_none() || magic.unwrap() != name {
        //     sequential_storage::erase_all(&mut flash_map.nor_flash, flash_map.addr_range.clone()).await?;
        //     flash_map.store(String::from(MAGIC_KEY), String::from(name)).await?;
        // }

        Ok(flash_map)
    }

    async fn init_flash_map(&mut self, name: &str) -> Result<(), Error<S::Error>> {
        const MAGIC_KEY: &str = "__map_name__";
        let magic = self.fetch(String::from(MAGIC_KEY)).await?;

        if magic.is_none() || magic.unwrap() != name {
            debug!("Existing flash map '{name}' not found, erasing and creating new");
            sequential_storage::erase_all(&mut self.nor_flash, self.addr_range.clone()).await?;
            self.store(String::from(MAGIC_KEY), String::from(name)).await?;
        }

        Ok(())
    }

    pub async fn new_in_region(nor_flash: S, region: &str, max_buf_size: usize, name: &str) -> Result<Self, Error<S::Error>> {
        let mut flash_map = Self {
            addr_range: Range { start: 0, end: 0 },
            nor_flash,
            max_buf_size,
            buffer: Vec::new(),
        };
        let partition_table = PartitionTable::default();
        let mut map_start: Option<u32> = None;
        let mut map_end: Option<u32> = None;
        partition_table.iter_storage(&mut flash_map, false).for_each(|partition| {
            if let Ok(partition) = partition {
                if partition.name() == "map" {
                    map_start = Some(partition.offset);
                    map_end = Some(partition.offset + partition.size as u32);
                }
            }
        });

        if let (Some(map_start), Some(map_end)) = (map_start, map_end) {
            flash_map.addr_range = Range {
                start: map_start,
                end: map_end,
            };
        } else {
            panic!("Region {} doen't exist", region);
        }

        flash_map.init_flash_map(name).await?;

        Ok(flash_map)
    }

    #[allow(dead_code)]
    pub fn save_memory(&mut self) {
        self.buffer.shrink_to(0);
    }

    pub async fn store(&mut self, key: String, value: String) -> Result<(), Error<S::Error>> {
        let len_for_this_operation = key.len() + value.len() + 2;
        if len_for_this_operation > self.max_buf_size {
            return Err(Error::ItemTooBig);
        }
        if self.buffer.len() < self.max_buf_size {
            self.buffer.resize(self.max_buf_size, 0)
        }

        sequential_storage::map::store_item::<String, String, _>(
            &mut self.nor_flash,
            self.addr_range.clone(),
            &mut NoCache::new(),
            &mut self.buffer,
            &key,
            &value,
        )
        .await?;

        Ok(())
    }

    pub async fn fetch(&mut self, key: String) -> Result<Option<String>, Error<S::Error>> {
        if self.buffer.len() < self.max_buf_size {
            self.buffer.resize(self.max_buf_size, 0)
        }

        sequential_storage::map::fetch_item::<String, String, _>(
            &mut self.nor_flash,
            self.addr_range.clone(),
            &mut NoCache::new(),
            &mut self.buffer,
            &key,
        )
        .await
    }

    pub async fn remove(&mut self, key: String) -> Result<(), Error<S::Error>> {
        if self.buffer.len() < self.max_buf_size {
            self.buffer.resize(self.max_buf_size, 0)
        }

        sequential_storage::map::remove_item::<String, _>(&mut self.nor_flash, self.addr_range.clone(), &mut NoCache::new(), &mut self.buffer, &key)
            .await
    }
}
