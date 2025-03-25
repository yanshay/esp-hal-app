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
    let mut buf = [0u8;4];
    getrandom::getrandom(&mut buf).unwrap();
    u32::from_le_bytes(buf)
}

pub fn random_u64() -> u64 {
    let mut buf = [0u8;8];
    getrandom::getrandom(&mut buf).unwrap();
    u64::from_le_bytes(buf)
}
