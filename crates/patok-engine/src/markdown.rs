//! Heading-delimited markdown sections: the shared splitter behind the plan
//! gate's section entries and the review stage's claims parsing.

/// One heading-delimited section of a markdown text.
#[derive(Debug, PartialEq)]
pub(crate) struct Section<'a> {
    /// The heading line as it appears in the text, leading whitespace included.
    pub heading: &'a str,
    /// Byte offset of the heading line's start within the text.
    pub start: usize,
    /// Byte offset where the section ends: the start of the next heading
    /// line, or the end of the text for the last section.
    pub end: usize,
}

/// The first section whose heading matches `name` (case-insensitively, any
/// level). `None` when the text has no such heading.
pub(crate) fn section<'a>(text: &'a str, name: &str) -> Option<Section<'a>> {
    let mut found: Option<(&str, usize)> = None;
    for line in text.lines() {
        let offset = line.as_ptr() as usize - text.as_ptr() as usize;
        let Some(line_name) = heading_name(line) else {
            continue;
        };
        if let Some((heading, start)) = found {
            return Some(Section {
                heading,
                start,
                end: offset,
            });
        }
        if line_name.eq_ignore_ascii_case(name) {
            found = Some((line, offset));
        }
    }
    found.map(|(heading, start)| Section {
        heading,
        start,
        end: text.len(),
    })
}

/// The name of a markdown heading line (`#` through `######`), or `None` for any other
/// line. The name is the heading text with the hashes and surrounding space stripped.
fn heading_name(line: &str) -> Option<&str> {
    let after_hashes = line.trim_start().strip_prefix('#')?.trim_start_matches('#');
    let name = after_hashes.trim();
    if name.is_empty() { None } else { Some(name) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heading_matches_at_any_level_case_insensitively() {
        // Leading whitespace before the hashes, trailing whitespace in the name.
        let text = "  ## build claims   \nbody";
        let found = section(text, "Build Claims").unwrap();
        assert_eq!(found.heading, "  ## build claims   ");
        assert_eq!(found.start, 0);
        assert!(section("#### BUILD CLAIMS\nbody", "build claims").is_some());
        assert!(section("# Build Claims\nbody", "Build Claims").is_some());
    }

    #[test]
    fn a_missing_section_or_a_text_without_headings_is_none() {
        assert_eq!(section("## Other\nbody", "Build Claims"), None);
        assert_eq!(section("just prose, no headings", "Build Claims"), None);
    }

    #[test]
    fn a_repeated_heading_picks_the_first_occurrence() {
        let text = "## Claims\nfirst\n## Claims\nsecond\n";
        let found = section(text, "Claims").unwrap();
        assert_eq!(found.start, 0);
        assert_eq!(found.end, "## Claims\nfirst\n".len());
        assert_eq!(&text[found.start..found.end], "## Claims\nfirst\n");
    }

    #[test]
    fn the_last_section_runs_to_the_end_of_the_text() {
        let text = "intro\n## Claims\nbody without a trailing newline";
        let found = section(text, "Claims").unwrap();
        assert_eq!(found.end, text.len());
        assert_eq!(
            &text[found.start..found.end],
            "## Claims\nbody without a trailing newline"
        );
    }

    #[test]
    fn a_bare_hash_line_is_body_not_a_boundary() {
        // A heading needs a name: bare hashes are ordinary section content.
        let text = "## Claims\n#\nbody\n###   \nmore\n## Other\n";
        let found = section(text, "Claims").unwrap();
        assert_eq!(
            &text[found.start..found.end],
            "## Claims\n#\nbody\n###   \nmore\n"
        );
    }

    #[test]
    fn a_matching_heading_with_an_empty_body_is_still_a_section() {
        let text = "## File Operations\n## Verification\nrun cargo test";
        let found = section(text, "File Operations").unwrap();
        assert_eq!(&text[found.start..found.end], "## File Operations\n");
    }

    #[test]
    fn the_bounds_slice_the_heading_and_body_exactly() {
        // Multi-byte characters before and inside the section prove the offsets
        // are byte offsets; blank lines are retained raw in the slice.
        let text = "intro ünïcode\n## Claims\nbody ✓ with blanks\n \n\t\n";
        let found = section(text, "Claims").unwrap();
        assert_eq!(found.start, "intro ünïcode\n".len());
        assert_eq!(
            &text[found.start..found.end],
            "## Claims\nbody ✓ with blanks\n \n\t\n"
        );
        assert!(text.is_char_boundary(found.start));
        assert!(text.is_char_boundary(found.end));
    }
}
