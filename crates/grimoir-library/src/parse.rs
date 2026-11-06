//! Filename metadata heuristics.

use std::path::Path;

/// A human-readable title derived from the file name.
pub fn title_from_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".to_owned());
    let cleaned = collapse_spaces(&stem.replace(['_', '-'], " "));
    if cleaned.is_empty() {
        "Untitled".to_owned()
    } else {
        cleaned
    }
}

/// Detects common volume markers such as `VOL. 03`, `Volume 3` and `v03`.
/// Returns the series prefix and the volume number.
pub fn parse_series(path: &Path) -> (Option<String>, Option<f64>) {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lower = stem.to_lowercase();

    let mut start = 0;
    while let Some(relative) = lower[start..].find(['v', 'V']) {
        let index = start + relative;
        let at_boundary = index == 0
            || !lower[..index]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric());
        if !at_boundary {
            start = index + 1;
            continue;
        }

        let rest = &lower[index..];
        let keyword_len = if rest.starts_with("volume") {
            6
        } else if rest.starts_with("vol") {
            3
        } else if rest.starts_with('v') {
            1
        } else {
            start = index + 1;
            continue;
        };

        let mut cursor = index + keyword_len;
        let bytes = lower.as_bytes();
        while cursor < bytes.len() && matches!(bytes[cursor], b' ' | b'.' | b'_' | b'-') {
            cursor += 1;
        }
        let digits_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor == digits_start {
            start = index + 1;
            continue;
        }

        let number = lower[digits_start..cursor].parse::<f64>().ok();
        let prefix = stem[..index]
            .trim()
            .trim_end_matches([':', '-', '(', '['])
            .trim()
            .to_owned();
        let series = if prefix.is_empty() {
            None
        } else {
            Some(prefix)
        };
        return (series, number);
    }

    (None, None)
}

fn collapse_spaces(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_volume_names() {
        let (series, index) = parse_series(Path::new("BLAME! VOL.01.pdf"));
        assert_eq!(series.as_deref(), Some("BLAME!"));
        assert_eq!(index, Some(1.0));

        let (series, index) = parse_series(Path::new("KOKOU NO HITO VOL.11.pdf"));
        assert_eq!(series.as_deref(), Some("KOKOU NO HITO"));
        assert_eq!(index, Some(11.0));

        let (series, index) = parse_series(Path::new("Vagabond v03.pdf"));
        assert_eq!(series.as_deref(), Some("Vagabond"));
        assert_eq!(index, Some(3.0));

        let (series, _) = parse_series(Path::new("Vagabond.pdf"));
        assert!(series.is_none());
    }

    #[test]
    fn cleans_titles() {
        assert_eq!(
            title_from_path(Path::new("Real-World  Bug_Hunting.pdf")),
            "Real World Bug Hunting"
        );
    }
}
