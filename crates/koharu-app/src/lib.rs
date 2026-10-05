//! Koharu's Tauri-managed application state, commands, and lifecycle.

mod app;
mod commands;
pub(crate) mod glossary;
pub(crate) mod injection;
pub(crate) mod webtoon;

pub use app::run;
pub use commands::bindings;
