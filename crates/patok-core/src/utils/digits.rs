//! The shared digit-run check of the version parser and the task-id splitter.

/// Whether `s` is a non-empty run of ASCII digits.
pub(crate) fn is_ascii_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}
