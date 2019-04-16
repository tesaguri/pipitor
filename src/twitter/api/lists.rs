api_requests! {
    GET "https://api.twitter.com/1.1/lists/statuses.json" => Vec<super::super::Tweet>;
    pub struct Statuses {
        list_id: u64;
        #[oauth1(option)]
        since_id: Option<i64>,
        #[oauth1(option)]
        max_id: Option<i64>,
        #[oauth1(option)]
        count: Option<usize> = Some(200),
        include_entities: bool,
    }
}
