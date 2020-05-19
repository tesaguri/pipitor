mod hub;
mod subscriber;

pub use subscriber::{Content, Subscriber};

const CALLBACK_PREFIX: &str = "/websub/callback/";
const X_HUB_SIGNATURE: &str = "x-hub-signature";
