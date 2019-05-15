#![feature(async_await)]

#[macro_use]
extern crate diesel;
#[macro_use]
extern crate log;

#[macro_use]
mod util;

pub mod manifest;
pub mod models;
pub mod rules;
pub mod schema;
pub mod twitter;

mod app;

pub use app::App;
pub use manifest::Manifest;
