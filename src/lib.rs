#![warn(clippy::all, rust_2018_idioms)]

mod app;
pub mod midi;
mod playback;

pub use app::MidiApp;
