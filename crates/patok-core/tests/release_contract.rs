//! Cross-check for the release contract, which is written four times: here in
//! `patok-core::update` (the reference), in `install.sh`, in the release
//! workflow and in the target triples of the CI workflow's cross-build
//! matrix. Fails when the target triples, the archive naming or the
//! checksum file naming drift apart.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use patok_core::update::{CHECKSUMS_ASSET, archive_name, supported_targets};

fn repo_file(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The values of every `target="<triple>"` assignment in install.sh
/// (the platform-detection case; the only place that string appears).
fn install_sh_targets(text: &str) -> BTreeSet<&str> {
    text.lines()
        .filter_map(|line| line.split("target=\"").nth(1)?.split('"').next())
        .collect()
}

/// The values of every `- target: <triple>` matrix entry in release.yml.
fn release_matrix_targets(text: &str) -> BTreeSet<&str> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("- target:").map(str::trim))
        .collect()
}

/// The value of `target` in every `- { ..., target: <triple> }` matrix
/// include entry in ci.yml. The other `target` spellings in that file
/// (`with: target:`, run-command paths, `matrix.target` references) do not
/// start with `- {` and are skipped.
fn ci_matrix_targets(text: &str) -> BTreeSet<&str> {
    text.lines()
        .filter_map(|line| {
            let entry = line.trim().strip_prefix("- {")?;
            let after = entry.split("target: ").nth(1)?;
            Some(after.trim().trim_end_matches('}').trim())
        })
        .collect()
}

/// Assert `actual` equals `expected`, naming any missing or extra triple.
fn assert_target_set(actual: &BTreeSet<&str>, expected: &BTreeSet<&str>, context: &str) {
    let missing: Vec<&str> = expected.difference(actual).copied().collect();
    let extra: Vec<&str> = actual.difference(expected).copied().collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{context} and patok-core supported_targets() disagree: \
         missing {missing:?}, extra {extra:?}"
    );
}

#[test]
fn ci_matrix_lists_exactly_the_supported_targets() {
    let text = repo_file(".github/workflows/ci.yml");
    let expected: BTreeSet<&str> = supported_targets().collect();
    let actual = ci_matrix_targets(&text);
    assert!(
        !actual.is_empty(),
        "ci.yml matrix entries are no longer one-line flow mappings"
    );
    assert_target_set(&actual, &expected, "the CI cross-build matrix");
}

#[test]
fn install_sh_detects_exactly_the_supported_targets() {
    let text = repo_file("install.sh");
    let expected: BTreeSet<&str> = supported_targets().collect();
    assert_eq!(
        install_sh_targets(&text),
        expected,
        "install.sh platform detection and patok-core supported_targets() disagree"
    );
}

#[test]
fn release_matrix_lists_exactly_the_supported_targets() {
    let text = repo_file(".github/workflows/release.yml");
    let expected: BTreeSet<&str> = supported_targets().collect();
    assert_eq!(
        release_matrix_targets(&text),
        expected,
        "the release build matrix and patok-core supported_targets() disagree"
    );
}

/// The `"..."` string starting right after the first occurrence of `marker`.
fn quoted_after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    let rest = &text[text.find(marker)? + marker.len()..];
    rest.split('"').next()
}

/// Substitute the placeholder (`$target` or `${{ matrix.target }}`) with `target`.
fn expand(pattern: &str, target: &str) -> String {
    pattern
        .replace("$target", target)
        .replace("${{ matrix.target }}", target)
}

#[test]
fn archive_names_agree_across_the_three_copies() {
    let install_sh = repo_file("install.sh");
    let release_yml = repo_file(".github/workflows/release.yml");
    // The quoted value of the `archive="..."` assignment in each file,
    // i.e. `patok-$target.tar.gz` and `patok-${{ matrix.target }}.tar.gz`.
    let install_pattern =
        quoted_after(&install_sh, "archive=\"").expect("install.sh assigns archive=");
    let release_pattern = quoted_after(&release_yml, "archive=\"")
        .expect("the release Package archive step assigns archive=");
    for target in supported_targets() {
        let expected = archive_name(target);
        assert_eq!(
            expand(install_pattern, target),
            expected,
            "install.sh archive pattern disagrees with archive_name for {target}"
        );
        assert_eq!(
            expand(release_pattern, target),
            expected,
            "release.yml archive pattern disagrees with archive_name for {target}"
        );
    }
}

#[test]
fn checksum_asset_naming_agrees_across_the_three_copies() {
    let install_sh = repo_file("install.sh");
    let release_yml = repo_file(".github/workflows/release.yml");
    // install.sh downloads the checksums asset by name and matches the
    // archive name exactly against the second field.
    assert!(
        install_sh.contains(&format!("\"$base/{CHECKSUMS_ASSET}\"")),
        "install.sh no longer downloads {CHECKSUMS_ASSET} from the release base URL"
    );
    assert!(
        install_sh.contains("'$2 == f { print $1; exit }'"),
        "install.sh checksum lookup is no longer an exact-filename match"
    );
    // The release workflow hashes the archives, writes one line per archive
    // under the same asset name, and publishes it.
    assert!(
        release_yml.contains("sha256sum patok-*.tar.gz"),
        "release.yml no longer hashes the patok-*.tar.gz archives"
    );
    for needle in [
        format!("> {CHECKSUMS_ASSET}"),
        format!("name: {CHECKSUMS_ASSET}"),
        format!("dist/{CHECKSUMS_ASSET}"),
    ] {
        assert!(release_yml.contains(&needle), "release.yml lost '{needle}'");
    }
    // One checksum line per supported target.
    let count_line = release_yml
        .lines()
        .find(|l| l.contains(&format!("wc -l < {CHECKSUMS_ASSET}")))
        .expect("release.yml checks the checksum line count");
    let count: usize = count_line
        .split("-eq")
        .nth(1)
        .and_then(|rest| {
            rest.split(|c: char| !c.is_ascii_digit())
                .find(|s| !s.is_empty())
        })
        .and_then(|d| d.parse().ok())
        .unwrap_or_else(|| panic!("cannot parse the archive count in: {count_line}"));
    assert_eq!(
        count,
        supported_targets().count(),
        "release.yml checksum line count and the supported target count disagree"
    );
}
