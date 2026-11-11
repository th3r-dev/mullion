//! The build's version, and ordering between release tags.
//!
//! CI sets `MULLION_VERSION` (`0.1.0-beta.7`) when it builds a release; a local
//! build reports the crate version (`0.1.0`). The update check compares this
//! against GitHub release tags (`v0.1.0-beta.8`).

/// This build's version, shown on screen and in the menu bar.
pub const VERSION: &str = match option_env!("MULLION_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// Is `candidate` a later version than `current`? Semver order: numbers first,
/// then a release beats any pre-release of the same numbers, and pre-release
/// parts compare numerically when both are numbers (`beta.10` > `beta.9`).
/// A leading `v` is ignored; anything that does not parse is never newer.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => cmp(&a, &b) == std::cmp::Ordering::Greater,
        _ => false,
    }
}

struct Version<'a> {
    core: [u64; 3],
    pre: Option<&'a str>,
}

fn parse(s: &str) -> Option<Version<'_>> {
    let s = s.trim().trim_start_matches('v');
    let s = s.split('+').next()?; // build metadata does not order
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) if !p.is_empty() => (c, Some(p)),
        Some(_) => return None,
        None => (s, None),
    };
    let mut n = core.split('.').map(|x| x.parse::<u64>().ok());
    let v = [n.next()??, n.next()??, n.next()??];
    if n.next().is_some() {
        return None;
    }
    Some(Version { core: v, pre })
}

fn cmp(a: &Version, b: &Version) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match a.core.cmp(&b.core) {
        Equal => {}
        o => return o,
    }
    match (a.pre, b.pre) {
        (None, None) => Equal,
        (None, Some(_)) => Greater,
        (Some(_), None) => Less,
        (Some(x), Some(y)) => {
            let (mut xs, mut ys) = (x.split('.'), y.split('.'));
            loop {
                match (xs.next(), ys.next()) {
                    (None, None) => return Equal,
                    (None, Some(_)) => return Less,
                    (Some(_), None) => return Greater,
                    (Some(p), Some(q)) => {
                        let o = match (p.parse::<u64>(), q.parse::<u64>()) {
                            (Ok(m), Ok(n)) => m.cmp(&n),
                            (Ok(_), Err(_)) => Less,
                            (Err(_), Ok(_)) => Greater,
                            (Err(_), Err(_)) => p.cmp(q),
                        };
                        if o != Equal {
                            return o;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn orders_release_tags() {
        assert!(is_newer("v0.1.0-beta.2", "0.1.0-beta.1"));
        assert!(is_newer("v0.1.0-beta.10", "0.1.0-beta.9"));
        assert!(is_newer("v0.1.0", "0.1.0-beta.9"));
        assert!(is_newer("v0.2.0-beta.1", "0.1.0"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("v0.1.0-beta.1", "0.1.0"));
        assert!(!is_newer("v0.1.0-beta.3", "0.1.0-beta.3"));
        assert!(!is_newer("v0.1.0-beta.2", "0.1.0-beta.10"));
        assert!(!is_newer("nightly", "0.1.0"));
        assert!(!is_newer("v0.1", "0.0.1"));
        assert!(!is_newer("v0.2.0", "garbage"));
    }
}
