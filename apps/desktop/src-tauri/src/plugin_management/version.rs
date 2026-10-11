//! Minimal SemVer precedence for OMP and adapter releases.

use std::cmp::Ordering;
use std::fmt;

const MAX_VERSION_BYTES: usize = 64;

/// A `MAJOR.MINOR.PATCH[-PRERELEASE][+BUILD]` version. Equality and order follow
/// SemVer precedence, so build metadata never distinguishes two releases.
#[derive(Clone, Debug)]
pub(super) struct Version {
    core: [u64; 3],
    prerelease: Vec<String>,
    text: String,
}

impl Version {
    pub(super) fn parse(text: &str) -> Option<Self> {
        if text.len() > MAX_VERSION_BYTES {
            return None;
        }
        let release = match text.split_once('+') {
            Some((release, build)) if identifiers(build) => release,
            Some(_) => return None,
            None => text,
        };
        let (core, prerelease) = match release.split_once('-') {
            Some((core, prerelease)) if identifiers(prerelease) => (core, Some(prerelease)),
            Some(_) => return None,
            None => (release, None),
        };
        let mut parts = core.split('.');
        let mut numbers = [0_u64; 3];
        for number in &mut numbers {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            *number = part.parse().ok()?;
        }
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            core: numbers,
            prerelease: prerelease
                .map(|prerelease| prerelease.split('.').map(str::to_owned).collect())
                .unwrap_or_default(),
            text: text.to_owned(),
        })
    }

    pub(super) fn is_prerelease(&self) -> bool {
        !self.prerelease.is_empty()
    }
}

fn identifiers(text: &str) -> bool {
    text.split('.').all(|identifier| {
        !identifier.is_empty()
            && identifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn numeric(identifier: &str) -> Option<u64> {
    if identifier.bytes().all(|byte| byte.is_ascii_digit()) {
        identifier.parse().ok()
    } else {
        None
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| {
            match (self.prerelease.is_empty(), other.prerelease.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (left, right) in self.prerelease.iter().zip(&other.prerelease) {
                        let order = match (numeric(left), numeric(right)) {
                            (Some(left), Some(right)) => left.cmp(&right),
                            (Some(_), None) => Ordering::Less,
                            (None, Some(_)) => Ordering::Greater,
                            (None, None) => left.cmp(right),
                        };
                        if order != Ordering::Equal {
                            return order;
                        }
                    }
                    self.prerelease.len().cmp(&other.prerelease.len())
                }
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}
