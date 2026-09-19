//! The only file here that touches the outside world, and the only one with
//! a platform `cfg`. Everything else is bytes in, numbers out.
//!
//! Keeping the I/O in one small file is what makes the rest testable from
//! vendored fixtures, and what makes adding a platform a one-file change.

#[cfg(unix)]
use std::path::PathBuf;

/// Which file a given `TZ` value names, or `None` if it names none.
///
/// Split out from the read so it can be tested without mutating the process
/// environment — which this crate's `unsafe_code = "deny"` would refuse
/// anyway, `set_var` having become `unsafe` in the 2024 edition. Taking the
/// value as a parameter is the better shape regardless: the interesting
/// logic is the mapping, not the getenv.
///
/// `TZ` wins when set, as it does for every other Unix program: a name is
/// looked up under `/usr/share/zoneinfo`, a leading `/` is an absolute path,
/// and a leading `:` is stripped (POSIX allows it and some tools emit it).
///
/// `TZ` can also hold a POSIX rule directly rather than a zone name
/// (`EST5EDT,M3.2.0,M11.1.0`). That form has no file behind it, so this
/// returns a path that will not open and the caller falls back — a
/// timestamp is dropped rather than wrong. Worth supporting one day; not
/// worth guessing at today.
#[cfg(unix)]
pub(crate) fn tz_path(tz: Option<&str>) -> Option<PathBuf> {
    let Some(tz) = tz.filter(|t| !t.is_empty()) else {
        return Some(PathBuf::from("/etc/localtime"));
    };
    let name = tz.strip_prefix(':').unwrap_or(tz);
    if name.is_empty() {
        return None;
    }
    if name.starts_with('/') {
        return Some(PathBuf::from(name));
    }
    // Reject anything that could climb out of the zoneinfo directory. `TZ`
    // is an environment variable, and a build tool should not read an
    // arbitrary file because one was set.
    if name.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return None;
    }
    Some(PathBuf::from("/usr/share/zoneinfo").join(name))
}

/// The system's TZif bytes, or `None` if there are none to be had.
#[cfg(unix)]
pub(crate) fn tzdata() -> Option<Vec<u8>> {
    let tz = std::env::var("TZ").ok();
    std::fs::read(tz_path(tz.as_deref())?).ok()
}

/// Windows keeps its zone in the registry, not in a TZif file, so there is
/// nothing for this reader to read. Reaching it needs either FFI — which
/// this module has none of, deliberately — or an embedded copy of the whole
/// tz database, which is how `jiff` does it at a measured cost of ~427 KB.
///
/// Neither is worth it for one line of output, so the caller drops the
/// timestamp and prints the rest. See the module docs for the plan to
/// revisit this once there is Windows CI to test it on (#85).
#[cfg(not(unix))]
pub(crate) fn tzdata() -> Option<Vec<u8>> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn unset_or_empty_means_the_system_link() {
        let sys = Some(PathBuf::from("/etc/localtime"));
        assert_eq!(tz_path(None), sys);
        assert_eq!(tz_path(Some("")), sys);
    }

    #[test]
    fn a_name_resolves_under_zoneinfo() {
        assert_eq!(
            tz_path(Some("Asia/Taipei")),
            Some(PathBuf::from("/usr/share/zoneinfo/Asia/Taipei"))
        );
        // POSIX allows a leading colon and some tools emit one.
        assert_eq!(
            tz_path(Some(":Asia/Taipei")),
            Some(PathBuf::from("/usr/share/zoneinfo/Asia/Taipei"))
        );
        // Three components happen (America/Argentina/Salta).
        assert_eq!(
            tz_path(Some("America/Argentina/Salta")),
            Some(PathBuf::from("/usr/share/zoneinfo/America/Argentina/Salta"))
        );
    }

    #[test]
    fn an_absolute_path_is_taken_as_given() {
        assert_eq!(
            tz_path(Some("/etc/localtime")),
            Some(PathBuf::from("/etc/localtime"))
        );
    }

    #[test]
    fn a_name_cannot_escape_the_zoneinfo_directory() {
        // Reading an arbitrary file because an environment variable said so
        // is a bad trade for a timestamp.
        for bad in [
            "../../../etc/passwd",
            "America/../../etc/passwd",
            ".",
            "..",
            "Asia/./Taipei",
            "Asia//Taipei",
            ":",
        ] {
            assert_eq!(tz_path(Some(bad)), None, "TZ={bad:?} should resolve to nothing");
        }
    }

    /// Not an assertion about the host's timezone — CI machines are UTC and
    /// developers are not — only that the lookup reaches a real file and
    /// that file parses. The values themselves are checked against vendored
    /// fixtures, where the expected answer is known.
    #[test]
    fn the_system_zone_is_readable_and_parses() {
        let Some(bytes) = tzdata() else {
            // A container with no tzdata installed is a legitimate state;
            // the caller handles it by dropping the timestamp.
            return;
        };
        assert!(
            super::super::tzif::TimeZone::parse(&bytes).is_some(),
            "the system's own tzdata did not parse ({} bytes)",
            bytes.len()
        );
    }
}
