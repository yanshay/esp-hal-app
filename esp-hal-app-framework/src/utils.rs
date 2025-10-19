use core::{future::Future, pin::Pin};

use alloc::boxed::Box;
use embassy_executor::{raw::TaskStorage, SpawnError, Spawner};

#[macro_export]
macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        // const SIZE: usize = core::mem::size_of::<$t>();
        // debug!(">>>{} -> {}", core::any::type_name::<$t>(), SIZE);
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

pub fn random_u32() -> u32 {
    let mut buf = [0u8; 4];
    getrandom::getrandom(&mut buf).unwrap();
    u32::from_le_bytes(buf)
}

pub fn random_u64() -> u64 {
    let mut buf = [0u8; 8];
    getrandom::getrandom(&mut buf).unwrap();
    u64::from_le_bytes(buf)
}


// Helper for using Snafu

pub struct DebugWrap<E>(pub E);

impl<E: core::fmt::Debug> core::error::Error for DebugWrap<E> {}

impl<E: core::fmt::Debug> core::fmt::Debug for DebugWrap<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl<E: core::fmt::Debug> core::fmt::Display for DebugWrap<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(&self.0, f)
    }
}

pub trait SpawnerHeapExt {
    fn spawn_heap<Fut>(&self, fut: Fut) -> Result<(), SpawnError>
    where
        Fut: Future<Output = ()> + 'static;
}

impl SpawnerHeapExt for Spawner {
    fn spawn_heap<Fut>(&self, fut: Fut) -> Result<(), SpawnError>
    where
        Fut: Future<Output = ()> + 'static,
    {
        let task = Box::leak(Box::new(TaskStorage::new())).spawn(|| fut);
        self.spawn(task)
    }
}

pub trait AwaitHeap: Future + Sized {
    fn await_heap(self) -> Pin<Box<Self>> {
        Box::pin(self)
    }
}

impl<F: Future + Sized> AwaitHeap for F {}
