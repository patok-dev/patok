//! The project brief the shell writes: the SPEC.md body a typed brief is
//! saved as, which provides the non-heading content the bootstrap gate
//! looks for. Nothing here touches the filesystem; the best-effort project
//! scan behind the idle scenario classification lives in
//! `patok_core::scenario`.

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
