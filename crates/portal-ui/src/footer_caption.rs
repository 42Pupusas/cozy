use std::path::Path;

pub struct FooterCaption;

impl FooterCaption {
    #[must_use]
    pub fn of(folder: &Path, query: &str) -> String {
        if query.is_empty() {
            folder.display().to_string()
        } else {
            format!("Filter: {query}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FooterCaption;
    use std::path::Path;

    #[test]
    fn without_a_query_the_caption_is_the_folder() {
        assert_eq!(
            FooterCaption::of(Path::new("/home/me/docs"), ""),
            "/home/me/docs"
        );
    }

    #[test]
    fn with_a_query_the_caption_shows_the_filter() {
        assert_eq!(
            FooterCaption::of(Path::new("/home/me/docs"), "rep"),
            "Filter: rep"
        );
    }
}
