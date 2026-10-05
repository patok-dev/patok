//! Project facts for the idle scenario classification:
//! the filesystem scan. The shell scans the project's files only; nothing here
//! can block the shell.

/// The SPEC.md body a typed brief is saved as: a heading plus the user's text, which
/// provides the non-heading content the bootstrap gate looks for.
pub fn brief_content(text: &str) -> String {
    format!("# Project brief\n\n{}\n", text.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_content_carries_the_text_under_a_heading() {
        assert_eq!(
            brief_content("  A web service for recipes.  "),
            "# Project brief\n\nA web service for recipes.\n"
        );
    }
}
