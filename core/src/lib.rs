// Rookery's core: configuration, the jj read model, the task store, the
// lifecycle verbs and the seat backends. The CLI and the TUI are frontends;
// this crate never prints to the terminal and never exits.
pub mod board;
pub mod config;
pub mod error;
pub mod jj;
pub mod lifecycle;
pub mod ops;
pub mod seat;
pub mod session;
pub mod task;
pub mod util;

pub use error::{Error, Result};
