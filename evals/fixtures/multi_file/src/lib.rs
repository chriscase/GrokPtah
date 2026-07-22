pub mod app;
pub mod config;

pub fn describe() -> String {
    crate::app::banner()
}
