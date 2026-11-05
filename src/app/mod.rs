//! The application layer between the front-ends (the TUI today, a web server
//! later) and the audio engine; see `docs/web-ui.md`.
//!
//! It starts with the session model and grows, step by step, into the
//! controller that applies intents to both the model and the engine.

pub mod apply;
pub mod catalog;
pub mod controller;
pub mod graph;
pub mod history;
pub mod intent;
pub mod journal;
pub mod layout;
pub mod load;
pub mod model;
pub mod module;
pub mod probes;
pub mod protocol;
pub mod save;
pub mod snapshot;
pub mod sources;
pub mod subscribers;
mod states;
pub mod windows;
pub mod wiring;
#[cfg(test)]
pub mod testutil;
