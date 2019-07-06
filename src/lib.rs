#![feature(async_await)]

#[macro_use]
extern crate diesel;
#[macro_use]
extern crate log;

#[macro_use]
mod util;

pub mod credentials;
pub mod manifest;
pub mod models;
pub mod rules;
pub mod schema;
pub mod twitter;

mod app;

pub use app::App;
pub use credentials::Credentials;
pub use manifest::Manifest;

#[doc(hidden)]
pub mod private {
    pub mod twitter {
        pub use crate::twitter::{api::*, *};
    }
}
