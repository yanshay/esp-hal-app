use embassy_time::Instant;

#[derive(Clone, Copy)]
pub struct BacklightConfig {
    pub dimming_timeout_secs: u64,
    pub dimming_percent: u8,
    pub blackout_timeout_secs: u64,
}

pub trait BacklightDevice {
    type Error;

    fn set_percent(&mut self, percent: u8) -> Result<(), Self::Error>;
}

pub struct BacklightController {
    last_touch_time: Instant,
    display_fully_dimmed: bool,
    display_partially_dimmed: bool,
    ignore_touch: bool,
}

impl BacklightController {
    pub fn new() -> Self {
        Self {
            last_touch_time: Instant::now(),
            display_fully_dimmed: false,
            display_partially_dimmed: false,
            ignore_touch: false,
        }
    }

    pub fn register_activity<D: BacklightDevice>(
        &mut self,
        backlight: &mut D,
    ) -> Result<(), D::Error> {
        self.last_touch_time = Instant::now();

        if self.display_partially_dimmed || self.display_fully_dimmed {
            backlight.set_percent(100)?;
            self.display_fully_dimmed = false;
            self.display_partially_dimmed = false;
        }

        Ok(())
    }

    pub fn tick<D: BacklightDevice>(
        &mut self,
        backlight: &mut D,
        config: BacklightConfig,
    ) -> Result<(), D::Error> {
        if !self.display_fully_dimmed
            && self.last_touch_time.elapsed().as_secs() > config.blackout_timeout_secs
        {
            backlight.set_percent(0)?;
            self.display_fully_dimmed = true;
            self.ignore_touch = true;
        } else if !self.display_partially_dimmed
            && self.last_touch_time.elapsed().as_secs() > config.dimming_timeout_secs
        {
            backlight.set_percent(config.dimming_percent)?;
            self.display_partially_dimmed = true;
        }

        Ok(())
    }

    pub fn ignoring_touch(&self) -> bool {
        self.ignore_touch
    }

    pub fn clear_ignore_touch(&mut self) {
        self.ignore_touch = false;
    }

    pub fn is_fully_dimmed(&self) -> bool {
        self.display_fully_dimmed
    }

    pub fn is_partially_dimmed(&self) -> bool {
        self.display_partially_dimmed
    }
}

impl Default for BacklightController {
    fn default() -> Self {
        Self::new()
    }
}
