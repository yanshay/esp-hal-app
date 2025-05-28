#[macro_export]
macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
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

