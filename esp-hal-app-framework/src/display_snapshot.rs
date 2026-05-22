use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use picoserve::response::chunked::{ChunkWriter, ChunksWritten};
use slint::platform::software_renderer::Rgb565Pixel;

use crate::framework::Framework;
use crate::slint_ext::SnapshotError;

const BMP_HEADER_LEN: usize = 54;

pub struct DisplaySnapshotBmp {
    width: u32,
    height: u32,
    pixels: Vec<Rgb565Pixel>,
    row_buffer: Vec<u8>,
    row_stride: usize,
}

#[derive(Debug)]
pub enum DisplaySnapshotError {
    Snapshot(SnapshotError),
    Allocation,
    InvalidDimensions,
}

impl From<SnapshotError> for DisplaySnapshotError {
    fn from(value: SnapshotError) -> Self {
        Self::Snapshot(value)
    }
}

impl DisplaySnapshotError {
    pub fn message(&self) -> String {
        match self {
            Self::Snapshot(err) => format!("Snapshot error: {err:?}"),
            Self::Allocation => String::from("Failed to allocate screenshot buffer"),
            Self::InvalidDimensions => String::from("Invalid screenshot dimensions"),
        }
    }
}

impl DisplaySnapshotBmp {
    pub const fn content_type() -> &'static str {
        "image/bmp"
    }

    pub fn take(framework: &Framework) -> Result<Self, DisplaySnapshotError> {
        let (width, height) = framework.display_snapshot_dimensions()?;
        let width_usize = width as usize;
        let height_usize = height as usize;
        let pixel_count = width_usize
            .checked_mul(height_usize)
            .ok_or(DisplaySnapshotError::InvalidDimensions)?;
        let row_stride = bmp_row_stride(width_usize).ok_or(DisplaySnapshotError::InvalidDimensions)?;
        let image_size = row_stride
            .checked_mul(height_usize)
            .ok_or(DisplaySnapshotError::InvalidDimensions)?;
        let file_size = BMP_HEADER_LEN
            .checked_add(image_size)
            .ok_or(DisplaySnapshotError::InvalidDimensions)?;
        if file_size > u32::MAX as usize {
            return Err(DisplaySnapshotError::InvalidDimensions);
        }

        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(pixel_count)
            .map_err(|_| DisplaySnapshotError::Allocation)?;
        pixels.resize(pixel_count, Rgb565Pixel(0));

        let mut row_buffer = Vec::new();
        row_buffer
            .try_reserve_exact(row_stride)
            .map_err(|_| DisplaySnapshotError::Allocation)?;
        row_buffer.resize(row_stride, 0);

        let (rendered_width, rendered_height) = framework.render_display_snapshot_rgb565(&mut pixels)?;
        if rendered_width != width || rendered_height != height {
            return Err(DisplaySnapshotError::InvalidDimensions);
        }

        Ok(Self {
            width,
            height,
            pixels,
            row_buffer,
            row_stride,
        })
    }

    pub async fn write_to<W: picoserve::io::Write>(
        mut self,
        chunk_writer: &mut ChunkWriter<W>,
    ) -> Result<(), W::Error> {
        let width = self.width as usize;
        let header = bmp_header(self.width, self.height, self.row_stride);
        chunk_writer.write_chunk(&header).await?;

        for row in self.pixels.chunks_exact(width) {
            for (pixel, target) in row.iter().zip(self.row_buffer.chunks_exact_mut(3)) {
                rgb565_to_bgr888(*pixel, target);
            }
            chunk_writer.write_chunk(&self.row_buffer).await?;
        }

        Ok(())
    }
}

impl picoserve::response::chunked::Chunks for DisplaySnapshotBmp {
    fn content_type(&self) -> &'static str {
        Self::content_type()
    }

    async fn write_chunks<W: picoserve::io::Write>(self, mut chunk_writer: ChunkWriter<W>) -> Result<ChunksWritten, W::Error> {
        self.write_to(&mut chunk_writer).await?;
        chunk_writer.finalize().await
    }
}

fn bmp_row_stride(width: usize) -> Option<usize> {
    width.checked_mul(3)?.checked_add(3).map(|v| v & !3)
}

fn write_u16_le(buffer: &mut [u8], offset: usize, value: u16) {
    buffer[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32_le(buffer: &mut [u8], offset: usize, value: u32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_i32_le(buffer: &mut [u8], offset: usize, value: i32) {
    buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn bmp_header(width: u32, height: u32, row_stride: usize) -> [u8; BMP_HEADER_LEN] {
    let image_size = row_stride * height as usize;
    let file_size = BMP_HEADER_LEN + image_size;
    let mut header = [0u8; BMP_HEADER_LEN];

    header[0] = b'B';
    header[1] = b'M';
    write_u32_le(&mut header, 2, file_size as u32);
    write_u32_le(&mut header, 10, BMP_HEADER_LEN as u32);
    write_u32_le(&mut header, 14, 40);
    write_i32_le(&mut header, 18, width as i32);
    write_i32_le(&mut header, 22, -(height as i32));
    write_u16_le(&mut header, 26, 1);
    write_u16_le(&mut header, 28, 24);
    write_u32_le(&mut header, 34, image_size as u32);

    header
}

fn rgb565_to_bgr888(pixel: Rgb565Pixel, output: &mut [u8]) {
    let raw = pixel.0;
    let r5 = (raw >> 11) & 0x1f;
    let g6 = (raw >> 5) & 0x3f;
    let b5 = raw & 0x1f;

    output[0] = ((b5 << 3) | (b5 >> 2)) as u8;
    output[1] = ((g6 << 2) | (g6 >> 4)) as u8;
    output[2] = ((r5 << 3) | (r5 >> 2)) as u8;
}
