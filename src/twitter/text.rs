pub const MAX_WEIGHTED_TWEET_LENGTH: usize = 280;
pub const TRANSFORMED_URL_LENGTH: usize = 23;

pub fn sanitize(s: &mut String, max_len: usize) {
    // TODO: make a more correct implementation.
    let max_len = max_len / 2;
    if s.chars().count() > max_len {
        *s = s.chars().take(max_len - 1).collect();
        s.push('…');
    }
}
