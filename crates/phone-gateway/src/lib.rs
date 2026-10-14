pub mod config;
pub mod fake_phone;
pub mod gateway;
pub mod output;
pub mod protocol;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

mod tunnel;

pub mod e2e;
