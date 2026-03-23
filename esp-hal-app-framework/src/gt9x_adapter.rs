use embassy_time::{Duration, Timer};
use gt9x::Chip;

use crate::touch::{Error, TouchAdapter, TouchEvent, TouchPosition};

pub type TouchCoordinateMapper = fn(TouchPosition) -> TouchPosition;

pub fn touch_identity_mapping(position: TouchPosition) -> TouchPosition {
    position
}

#[derive(Clone, Copy)]
pub struct Gt9xAdapterConfig {
    pub polling_timeout: Duration,
    pub missing_points_before_release: u8,
    pub coordinate_mapper: TouchCoordinateMapper,
}

impl Default for Gt9xAdapterConfig {
    fn default() -> Self {
        Self {
            polling_timeout: Duration::from_millis(40),
            missing_points_before_release: 2,
            coordinate_mapper: touch_identity_mapping,
        }
    }
}

pub struct Jc8048w550cGt911;

impl Chip for Jc8048w550cGt911 {
    const ID: &str = "911\0";
    const MAX_POINTS: u8 = 1;
}

pub struct Gt9xAdapter<'a, I2C>
where
    I2C: embedded_hal_async::i2c::I2c,
{
    driver: gt9x::Gt9x<'a, Jc8048w550cGt911, I2C, (), gt9x::NoIntPin, ()>,
    config: Gt9xAdapterConfig,
    last_position: Option<TouchPosition>,
    currently_pressed: bool,
    missing_points_count: u8,
    consecutive_read_errors: u32,
}

impl<'a, I2C> Gt9xAdapter<'a, I2C>
where
    I2C: embedded_hal_async::i2c::I2c,
{
    pub fn new(
        driver: gt9x::Gt9x<'a, Jc8048w550cGt911, I2C, (), gt9x::NoIntPin, ()>,
        config: Gt9xAdapterConfig,
    ) -> Self {
        Self {
            driver,
            config,
            last_position: None,
            currently_pressed: false,
            missing_points_count: 0,
            consecutive_read_errors: 0,
        }
    }
}

impl<'a, I2C> TouchAdapter for Gt9xAdapter<'a, I2C>
where
    I2C: embedded_hal_async::i2c::I2c,
{
    async fn next_event(&mut self) -> Result<TouchEvent, Error> {
        loop {
            let points = match self.driver.get_touches().await {
                Ok(points) => {
                    self.consecutive_read_errors = 0;
                    points
                }
                Err(err) => {
                    self.consecutive_read_errors = self.consecutive_read_errors.saturating_add(1);
                    if self.consecutive_read_errors == 1
                        || self.consecutive_read_errors % 20 == 0
                    {
                        warn!(
                            "GT9x read error ({} consecutive): {:?}",
                            self.consecutive_read_errors,
                            err
                        );
                    }
                    Timer::after(self.config.polling_timeout).await;
                    continue;
                }
            };

            if points.is_empty() {
                if self.currently_pressed {
                    self.missing_points_count = self.missing_points_count.saturating_add(1);

                    if self.missing_points_count >= self.config.missing_points_before_release {
                        if let Some(last_position) = self.last_position {
                            self.currently_pressed = false;
                            self.last_position = None;
                            self.missing_points_count = 0;
                            return Ok(TouchEvent::TouchReleased(last_position));
                        }
                    }
                }

                Timer::after(self.config.polling_timeout).await;
                continue;
            }

            self.missing_points_count = 0;

            let point = &points[0];
            let mapped_position = (self.config.coordinate_mapper)(TouchPosition {
                x: point.x as i32,
                y: point.y as i32,
            });

            if !self.currently_pressed {
                self.currently_pressed = true;
                self.last_position = Some(mapped_position);
                return Ok(TouchEvent::TouchPressed(mapped_position));
            }

            if self.last_position != Some(mapped_position) {
                self.last_position = Some(mapped_position);
                return Ok(TouchEvent::TouchMoved(mapped_position));
            }

            Timer::after(self.config.polling_timeout).await;
        }
    }
}
