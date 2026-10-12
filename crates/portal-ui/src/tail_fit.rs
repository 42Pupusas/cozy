pub struct TailFit;

impl TailFit {
    const ELLIPSIS: char = '…';

    #[must_use]
    pub fn fit(text: &str, width: i32, mut measure: impl FnMut(&str) -> i32) -> String {
        if width <= 0 {
            return String::new();
        }
        if measure(text) <= width {
            return text.to_owned();
        }
        let chars: Vec<char> = text.chars().collect();
        for skip in 1..=chars.len() {
            let candidate: String = std::iter::once(Self::ELLIPSIS)
                .chain(chars[skip..].iter().copied())
                .collect();
            if measure(&candidate) <= width {
                return candidate;
            }
        }
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::TailFit;

    struct Cells;

    impl Cells {
        fn wide(text: &str) -> i32 {
            i32::try_from(text.chars().count()).unwrap_or(i32::MAX)
        }
    }

    #[test]
    fn text_that_fits_is_unchanged() {
        assert_eq!(TailFit::fit("/home/me", 20, Cells::wide), "/home/me");
    }

    #[test]
    fn long_text_loses_its_head_and_keeps_its_tail() {
        assert_eq!(
            TailFit::fit("/home/me/projects", 9, Cells::wide),
            "…projects"
        );
    }

    #[test]
    fn the_result_never_exceeds_the_width() {
        for width in 0..20 {
            let fitted = TailFit::fit("/home/me/projects/cozy", width, Cells::wide);
            assert!(
                Cells::wide(&fitted) <= width.max(0),
                "{fitted:?} at {width}"
            );
        }
    }

    #[test]
    fn no_room_gives_nothing() {
        assert_eq!(TailFit::fit("/home", 0, Cells::wide), "");
        assert_eq!(TailFit::fit("/home", -3, Cells::wide), "");
    }

    #[test]
    fn multibyte_text_is_cut_on_characters() {
        assert_eq!(TailFit::fit("/home/日本語の庭", 4, Cells::wide), "…語の庭");
    }
}
