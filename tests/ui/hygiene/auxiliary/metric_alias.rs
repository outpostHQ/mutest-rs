#![crate_type = "lib"]

mod builder {
    pub struct Counter;
    pub struct Gauge;
}

pub mod core {
    pub struct MetricVec<B> {
        pub(crate) builder: B,
        pub(crate) len: u8,
    }

    impl<B> MetricVec<B> {
        pub fn len(&self) -> u8 { self.len }
    }
}

impl core::MetricVec<builder::Counter> {
    pub fn new(len: u8) -> Self { core::MetricVec { builder: builder::Counter, len } }
}

impl core::MetricVec<builder::Gauge> {
    pub fn new(len: u8) -> Self { core::MetricVec { builder: builder::Gauge, len } }
}

impl Default for core::MetricVec<builder::Counter> {
    fn default() -> Self { Self::new(0) }
}

impl Default for core::MetricVec<builder::Gauge> {
    fn default() -> Self { Self::new(0) }
}

pub type CounterVec = core::MetricVec<builder::Counter>;
pub type GaugeVec = core::MetricVec<builder::Gauge>;
