use embassy_time::Duration;

use crate::touch::{Error, IrqTraits, TouchAdapter, TouchEvent, TouchPosition};

pub struct Ft6x36TouchAdapter<IRQ, I2C> {
    irq: IRQ,
    driver: ft6x36::Ft6x36<I2C>,
    last_returned_event: Option<TouchEvent>,
}

// use embedded_hal

impl<IRQ, I2C> Ft6x36TouchAdapter<IRQ, I2C>
where
    I2C: embedded_hal::i2c::I2c<embedded_hal::i2c::SevenBitAddress>,
    IRQ: IrqTraits,
{
    pub fn new(driver: ft6x36::Ft6x36<I2C>, irq: IRQ) -> Self {
        Self {
            irq,
            driver,
            last_returned_event: None,
        }
    }

    fn event(&mut self) -> Result<Option<TouchEvent>, Error> {
        let t = self
            .driver
            .get_touch_event()
            .expect("Failed to read ft6x36 touch event");
        // dbg!(t);

        match t.p1 {
            None => {
                if let Some(event) = self.last_returned_event {
                    self.last_returned_event = None;
                    Ok(Some(TouchEvent::TouchReleased(event.touch_position())))
                } else {
                    Ok(None)
                }
            }
            Some(event) => {
                let ft6x36::TouchPoint { touch_type, x, y } = event;
                let pos = TouchPosition {
                    x: x as i32,
                    y: y as i32,
                };
                match touch_type {
                    ft6x36::TouchType::Press => {
                        self.last_returned_event = Some(TouchEvent::TouchPressed(pos));
                        Ok(self.last_returned_event)
                    }
                    ft6x36::TouchType::Contact => {
                        // if starting with a move event, then missed the press, it is more important then sending it
                        // Theoretically, there should have been a queue
                        if self.last_returned_event.is_none() {
                            self.last_returned_event = Some(TouchEvent::TouchPressed(pos));
                            Ok(self.last_returned_event)
                        } else {
                            self.last_returned_event = Some(TouchEvent::TouchMoved(pos));
                            Ok(self.last_returned_event)
                        }
                    }
                    ft6x36::TouchType::Release => {
                        self.last_returned_event = None;
                        Ok(Some(TouchEvent::TouchReleased(pos)))
                    }
                    ft6x36::TouchType::Invalid => Err(Error::IOError),
                }
            }
        }
    }
}

impl<IRQ, I2C> TouchAdapter for Ft6x36TouchAdapter<IRQ, I2C>
where
    I2C: embedded_hal::i2c::I2c<embedded_hal::i2c::SevenBitAddress>,
    IRQ: IrqTraits,
{
    //  TODO: potentially can add noise reduction, after release, wait a period of time before
    //  allowing to generate events, so there won't be a too quick press/up/press/up
    //  TODO: to the reading also async (not sure it's worth it though)
    // #[cfg(feature = "async")]
    async fn next_event(&mut self) -> Result<TouchEvent, Error> {
        use embassy_time::with_timeout;

        loop {
            if self.last_returned_event.is_some() {
                // if touch is already pressed, wait for either (a) release of touch or (b) timeout
                // in other words, start polling and check if need to generate a touch event (on move, or release) every x millisec
                // and if a release of the interrupt line (meaning depress) happens earlier response will be faster than the x millisec polling
                let wait_res = with_timeout(Duration::from_millis(200), self.irq.wait_for_high()).await;
                if let Ok(irq_wait_res) = wait_res {
                    if irq_wait_res.is_err() {
                        panic!("Touch IRQ wait_for_high failed");
                    }
                }
            } else {
                // if touch not already pressed,
                // let's not waste cpu by polling, wait for the first event to trigger the press
                // in this case we wait for low (rather than transition) in case for some reason we miss the edge or data
                // not available on edge, so a low signal will trigger again
                if self.irq.wait_for_low().await.is_err() {
                    panic!("Touch IRQ wait_for_low failed");
                }
            }

            match self.event() {
                Ok(event) => {
                    if let Some(event) = event {
                        return Ok(event);
                    } else {
                        // in case of no event for some reason
                        continue;
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }
}
