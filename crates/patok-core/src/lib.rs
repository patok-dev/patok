//! Placeholder for the `patok-core` crate (see spec Part X, section 3).

/// Crate name, used to prove the workspace links.
pub const NAME: &str = "patok-core";

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_crate() {
        assert_eq!(super::NAME, env!("CARGO_PKG_NAME"));
    }
}
