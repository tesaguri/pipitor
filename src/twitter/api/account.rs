api_requests! {
    GET "https://api.twitter.com/1.1/account/verify_credentials.json" => super::super::User;
    pub struct VerifyCredentials {;}
}
