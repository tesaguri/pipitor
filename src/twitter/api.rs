macro_rules! api_requests {
    (
        $method:ident $uri:expr => $Data:ty;
        $(#[$attr:meta])*
        $vis:vis struct $Name:ident $(<$($lt:lifetime),*>)? {
            $($(#[$req_attr:meta])* $required:ident: $req_ty:ty),*;
            $($(#[$opt_attr:meta])* $optional:ident: $opt_ty:ty $(= $default:expr)?),* $(,)?
        }
        $($rest:tt)*
    ) => {
        $(#[$attr])*
        #[derive(oauth1::Request)]
        $vis struct $Name $(<$($lt),*>)? {
            $($(#[$req_attr])* $required: $req_ty,)*
            $($(#[$opt_attr])* $optional: $opt_ty,)*
        }

        impl $(<$($lt),*>)? $Name $(<$($lt),*>)? {
            pub fn new($($required: $req_ty),*) -> Self {
                #[allow(unused_macros)]
                macro_rules! this_or_default {
                    ($this:expr) => ($this);
                    () => (Default::default());
                }

                $Name {
                    $($required,)*
                    $($optional: this_or_default!($($default)?),)*
                }
            }

            $(
                #[allow(dead_code)]
                pub fn $optional(&mut self, $optional: $opt_ty) -> &mut Self {
                    self.$optional = $optional;
                    self
                }
            )*
        }

        impl $(<$($lt),*>)? twitter_client::request::RawRequest for $Name $(<$($lt),*>)? {
            fn method(&self) -> &http::Method {
                &http::Method::$method
            }

            fn uri(&self) -> &'static str {
                $uri
            }
        }

        impl $(<$($lt),*>)? twitter_client::Request for $Name $(<$($lt),*>)? {
            type Data = $Data;
        }

        api_requests! { $($rest)* }
    };
    () => ();
}

pub mod account;
pub mod lists;
pub mod statuses;
