//! Version parsing and semver-precedence comparison.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

/// A parsed version: numeric core plus an optional pre-release suffix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    /// Dot-separated pre-release fields; empty for a stable release.
    pub pre: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseVersionError(pub String);

impl fmt::Display for ParseVersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid version: {}", self.0)
    }
}

impl std::error::Error for ParseVersionError {}

impl FromStr for Version {
    type Err = ParseVersionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = || ParseVersionError(s.to_string());
        let t = s.trim();
        let t = t.strip_prefix('v').unwrap_or(t);
        let (core, pre) = match t.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (t, None),
        };
        let mut parts = core.split('.');
        let mut num = || -> Result<u64, ParseVersionError> {
            let p = parts.next().ok_or_else(err)?;
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(err());
            }
            p.parse().map_err(|_| err())
        };
        let (major, minor, patch) = (num()?, num()?, num()?);
        if parts.next().is_some() {
            return Err(err());
        }
        let pre = match pre {
            None => Vec::new(),
            Some(p) => {
                let fields: Vec<String> = p.split('.').map(str::to_string).collect();
                if fields.iter().any(String::is_empty) {
                    return Err(err());
                }
                fields
            }
        };
        Ok(Version {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

fn is_numeric(f: &str) -> bool {
    f.bytes().all(|b| b.is_ascii_digit())
}

fn cmp_field(a: &str, b: &str) -> Ordering {
    match (is_numeric(a), is_numeric(b)) {
        (true, true) => {
            // Compare by magnitude without overflow: strip leading zeros, then length, then lexically.
            let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
            a.len().cmp(&b.len()).then_with(|| a.cmp(b))
        }
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.cmp(b),
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let o = cmp_field(a, b);
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    #[test]
    fn parses_and_ignores_leading_v() {
        assert_eq!(v("v1.2.3"), v("1.2.3"));
        assert_eq!(v("1.2.3-beta.1").pre, vec!["beta", "1"]);
        assert_eq!(v("v1.2.0-rc.2").to_string(), "1.2.0-rc.2");
    }

    #[test]
    fn rejects_malformed() {
        for s in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "a.b.c",
            "1.2.x",
            "1.2.3-",
            "1.2.3-a..b",
            "vv1.2.3",
            "1..3",
        ] {
            assert!(s.parse::<Version>().is_err(), "{s}");
        }
    }

    #[test]
    fn core_compares_numerically() {
        assert!(v("1.10.0") > v("1.9.0"));
        assert!(v("2.0.0") > v("1.99.99"));
        assert!(v("1.2.10") > v("1.2.9"));
    }

    #[test]
    fn release_outranks_own_prerelease() {
        assert!(v("1.2.0") > v("1.2.0-rc.1"));
        assert!(v("1.2.0-rc.1") > v("1.1.9"));
    }

    #[test]
    fn prerelease_fields() {
        assert!(v("1.0.0-beta.2") > v("1.0.0-beta.1"));
        assert!(v("1.0.0-beta.11") > v("1.0.0-beta.2"));
        assert!(v("1.0.0-rc.1") > v("1.0.0-beta.1"));
        assert!(v("1.0.0-alpha.beta") > v("1.0.0-alpha.1"));
        assert!(v("1.0.0-alpha.1") > v("1.0.0-alpha"));
        assert!(v("1.0.0-beta") > v("1.0.0-alpha.9"));
    }

    #[test]
    fn equal_ignoring_v() {
        assert_eq!(v("v1.0.0-rc.1").cmp(&v("1.0.0-rc.1")), Ordering::Equal);
    }

    #[test]
    fn huge_numeric_fields_do_not_overflow() {
        assert!(v("1.0.0-99999999999999999999999") > v("1.0.0-9999999999999999999999"));
    }

    #[test]
    fn comparison_table() {
        use Ordering::{Equal, Greater, Less};
        let table = [
            ("1.0.0", "1.0.0", Equal),
            ("v1.0.0", "1.0.0", Equal),
            ("1.0.1", "1.0.0", Greater),
            ("1.0.0", "1.0.1", Less),
            ("1.10.0", "1.9.0", Greater),
            ("2.0.0", "1.99.99", Greater),
            ("0.9.9", "1.0.0", Less),
            ("1.0.0", "1.0.0-rc.1", Greater),
            ("1.0.0-rc.1", "1.0.0", Less),
            ("1.0.0-rc.2", "1.0.0-rc.10", Less),
            ("1.0.0-alpha", "1.0.0-beta", Less),
            ("1.0.0-beta", "1.0.0-beta.1", Less),
            ("1.0.0-rc.1", "v1.0.0-rc.1", Equal),
        ];
        for (a, b, expected) in table {
            assert_eq!(v(a).cmp(&v(b)), expected, "{a} vs {b}");
        }
    }
}
