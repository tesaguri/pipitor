#![feature(async_await, await_macro, futures_api)]

#[macro_use]
extern crate diesel;
extern crate oauth1_request as oauth1;
extern crate serde_json as json;
#[macro_use]
extern crate log;
#[macro_use]
extern crate tokio_async_await;

macro_rules! trace_fn {
    (@heading $path:path) => {
        concat!(file!(), ':', line!(), ':', column!(), ' ', stringify!($path));
    };
    ($path:path) => {{
        // Ensure that at least the symbol `$path` exists.
        #[allow(path_statements)] { $path; }
        trace!(trace_fn!(@heading $path));
    }};
    ($path:path, $fmt:tt $($arg:tt)*) => {{
        #[allow(path_statements)] { $path; }
        trace!(concat!(trace_fn!(@heading $path), "; ", $fmt) $($arg)*);
    }};
}

pub mod manifest;
pub mod models;
pub mod rules;
pub mod schema;
pub mod twitter;

mod app;

pub use app::App;
pub use manifest::Manifest;
