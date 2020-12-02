#[macro_use]
extern crate diesel;
#[macro_use]
extern crate log;
#[macro_use]
extern crate diesel_migrations;

#[macro_use]
mod util;

pub mod manifest;
pub mod migrations;
pub mod models;
pub mod router;
#[rustfmt::skip]
pub mod schema;
pub mod socket;
pub mod twitter;

mod app;
mod feed;
mod query;
mod websub;

pub use app::App;
pub use manifest::Manifest;

#[doc(hidden)]
pub mod private {
    pub mod twitter {
        pub use crate::twitter::{api::*, *};
    }

    pub mod util {
        pub use crate::util::r2d2;
    }
}
