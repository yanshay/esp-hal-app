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

#[allow(async_fn_in_trait)]
pub trait TouchAdapter {
    async fn next_event(&mut self) -> Result<TouchEvent, Error>;
}

pub struct Touch<A> {
    adapter: A,
}

impl<A> Touch<A>
where
    A: TouchAdapter,
{
    pub fn new(adapter: A) -> Self {
        Self { adapter }
    }

    pub async fn event_async(&mut self) -> Result<Option<TouchEvent>, Error> {
        self.adapter.next_event().await.map(Some)
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
