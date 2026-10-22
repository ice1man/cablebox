//! Minimal async client for the Tvheadend HTTP API.

mod client;
mod model;

pub use client::{Auth, Client, Error, Result};
pub use model::*;
