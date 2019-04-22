api_requests! {
    GET "https://api.twitter.com/1.1/account/verify_credentials.json" => super::super::User;
    #[derive(Default)]
    pub struct VerifyCredentials {;}
}
