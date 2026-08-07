//! 网络设备驱动
#[cfg(feature = "extra-drivers")]
pub mod e1000;
#[cfg(not(feature = "extra-drivers"))]
pub mod e1000 {
    pub struct E1000 { pub mmio_base: usize, pub mac: [u8; 6], pub irq: u8, pub initialized: bool }
    impl E1000 {
        pub fn probe() -> Option<*mut E1000> { None }
        pub fn get() -> Option<*mut E1000> { None }
        pub fn has_packet(&self) -> bool { false }
        pub fn send(&mut self, _data: &[u8]) -> i32 { -1 }
        pub fn receive(&mut self, _buf: &mut [u8]) -> Option<usize> { None }
        pub fn selftest() {}
    }
}
