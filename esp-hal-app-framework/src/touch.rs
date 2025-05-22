use embassy_time::Duration;
use embedded_hal::digital::InputPin;

pub enum Error {
    /// Some error originating from the communication bus
    // BusError(E),
    /// The message length did not match the expected value
    // InvalidMessageLen(usize),
    /// Reading a GPIO pin resulted in an error
    IOError,
    // Tried to read a touch point, but no data was available
    // NoDataAvailable,
    // Error converting a slice to an array
    // TryFromSliceError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TouchPosition {
    pub x: i32,
    pub y: i32,
    // pub z1: i32,
    // pub z2: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum TouchEvent {
    TouchPressed(TouchPosition),
    TouchReleased(TouchPosition),
    TouchMoved(TouchPosition),
}
impl TouchEvent {
    pub fn touch_position(&self) -> TouchPosition {
        match *self {
            TouchEvent::TouchPressed(pos) => pos,
            TouchEvent::TouchReleased(pos) => pos,
            TouchEvent::TouchMoved(pos) => pos,
        }
    }
}

pub trait IrqTraits = InputPin + embedded_hal_async::digital::Wait;

pub struct Touch<IRQ, I2C> {
    irq: IRQ,
    driver: ft6x36::Ft6x36<I2C>,
    last_returned_event: Option<TouchEvent>,
}

// use embedded_hal

impl<IRQ, I2C> Touch<IRQ, I2C>
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

    pub fn event(&mut self) -> Result<Option<TouchEvent>, Error> {
        let t = self.driver.get_touch_event().unwrap();
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

    //  TODO: potentially can add noise reduction, after release, wait a period of time before
    //  allowing to generate events, so there won't be a too quick press/up/press/up
    //  TODO: to the reading also async (not sure it's worth it though)
    // #[cfg(feature = "async")]
    pub async fn event_async(&mut self) -> Result<Option<TouchEvent>, Error> {
        use embassy_time::with_timeout;

        loop {
            if self.last_returned_event.is_some() {
                // if touch is already pressed, wait for either (a) release of touch or (b) timeout
                // in other words, start polling and check if need to generate a touch event (on move, or release) every x millisec
                // and if a release of the interrupt line (meaning depress) happens earlier response will be faster than the x millisec polling
                let _ = with_timeout(Duration::from_millis(200), self.irq.wait_for_high()).await;
            } else {
                // if touch not already pressed,
                // let's not waste cpu by polling, wait for the first event to trigger the press
                // in this case we wait for low (rather than transition) in case for some reason we miss the edge or data
                // not available on edge, so a low signal will trigger again
                let _ = self.irq.wait_for_low().await;
            }
            match self.event() {
                Ok(event) => {
                    if event.is_some() {
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

    // https://stackoverflow.com/questions/66607516/how-to-implement-streams-from-future-functions
    // #[cfg(feature = "async")]
    pub fn events_stream_async(
        &mut self,
    ) -> impl futures::Stream<Item = Result<Option<TouchEvent>, Error>> + '_ {
        futures::stream::unfold(self, |rng| async {
            let event = rng.event_async().await;
            Some((event, rng))
        })
    }
}
