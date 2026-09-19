//! Path spelling rules as a value, so the Windows ones can be tested anywhere.
//!
//! `std::path` splits and compares by the HOST's rules, which left every
//! Windows-only path rule unreachable from a test that does not run on Windows:
//! on macOS `c:\a\b` is a single `Component`, so the Windows branch of a
//! relativisation could not be exercised at all. #146 lived in exactly that
//! blind spot — two relativisations compared a lowercased canonical key against
//! a mixed-case `current_dir()`, matched nothing past the drive letter, and
//! quietly printed absolute paths in every diagnostic instead.
//!
//! dart's `path` package models this as `Style.posix` / `Style.windows` on an
//! explicit `Context`, which is how its own suite checks Windows behaviour on a
//! POSIX host. This is the same idea, narrowed to the one operation sasso
//! needs: the relative path from one absolute path to another.

/// Which platform's rules a path is read and written by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Style {
    /// `/` separates, and two names differing in case are two files.
    Posix,
    /// `\` separates for output and `/` is read as one too; two names differing
    /// in ASCII case are ONE file, and a drive letter belongs to the root
    /// rather than being a segment.
    Windows,
}

/// The rules of the platform this build runs on.
pub(crate) const HOST: Style = if cfg!(windows) {
    Style::Windows
} else {
    Style::Posix
};

impl Style {
    /// The separator to spell a path WITH. Windows reads `/` as a separator too
    /// (see [`Style::is_sep`]), but dart writes `\` there, so a diagnostic path
    /// does.
    pub(crate) fn sep(self) -> &'static str {
        match self {
            Style::Posix => "/",
            Style::Windows => "\\",
        }
    }

    /// Whether `c` separates two segments.
    fn is_sep(self, c: char) -> bool {
        c == '/' || (self == Style::Windows && c == '\\')
    }

    /// The byte length of `p`'s absolute root — `0` when `p` is relative.
    ///
    /// Windows has four spellings: a drive (`C:\`), a UNC share
    /// (`\\server\share\`), the current drive's root (`\`), and a verbatim or
    /// device wrapper around a drive (`\\?\C:\`, `\\.\C:\`) — which
    /// `std::fs::canonicalize` produces and `current_dir` never does.
    fn root_len(self, p: &str) -> usize {
        if self == Style::Posix {
            return usize::from(p.starts_with('/'));
        }
        let skip = if p.starts_with(r"\\?\") || p.starts_with(r"\\.\") {
            4
        } else {
            0
        };
        let rest = &p[skip..];
        let b = rest.as_bytes();
        // A drive letter is a root only with a separator after it. `C:foo` is
        // DRIVE-RELATIVE — the current directory on C:, which a process tracks
        // per drive — so it has no root at all and nothing can be measured
        // from it.
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            let sep = b.get(2).is_some_and(|&c| self.is_sep(c as char));
            return if sep { skip + 3 } else { 0 };
        }
        // A UNC share: two separators, a server, a separator, a share. The
        // share is part of the root — `\\a\b` and `\\a\c` share no directory.
        if skip == 0 && b.len() >= 2 && self.is_sep(b[0] as char) && self.is_sep(b[1] as char) {
            let mut end = 2;
            for _ in 0..2 {
                end += rest[end..].find(|c| self.is_sep(c)).unwrap_or(rest.len() - end);
                if end < rest.len() {
                    end += 1;
                }
            }
            return end;
        }
        // The current drive's root.
        usize::from(b.first().is_some_and(|&c| self.is_sep(c as char)))
    }

    /// `p`'s absolute root, or `None` when `p` is relative.
    fn root(self, p: &str) -> Option<&str> {
        let n = self.root_len(p);
        (n > 0).then(|| &p[..n])
    }

    /// `p`'s non-empty segments, with its root and any repeated separators
    /// dropped.
    pub(crate) fn segments<'a>(self, p: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        p[self.root_len(p)..]
            .split(move |c| self.is_sep(c))
            .filter(|s| !s.is_empty())
    }

    /// Whether two absolute roots name the same one. On Windows a drive letter
    /// is case-insensitive, `/` and `\` are the same separator, and a verbatim
    /// or device marker is not part of the identity: `std::fs::canonicalize`
    /// emits one where `current_dir` does not, so the two sides of a comparison
    /// routinely disagree about it.
    ///
    /// A trailing separator is not part of the identity either: a share is
    /// spelled `\\server\share` when it is the whole path and
    /// `\\server\share\` when something follows, so the two sides of a
    /// comparison disagree about it whenever the working directory IS the
    /// share.
    ///
    /// The verbatim UNC form (`\\?\UNC\server\share`) is NOT unwrapped —
    /// nothing here produces it.
    fn roots_eq(self, a: &str, b: &str) -> bool {
        if self == Style::Posix {
            return a == b;
        }
        fn bare(style: Style, r: &str) -> &str {
            let r = match r.strip_prefix(r"\\?\").or_else(|| r.strip_prefix(r"\\.\")) {
                // Only a drive root survives the unwrap: see the note above.
                Some(inner) if style.root_len(inner) == inner.len() => inner,
                _ => r,
            };
            // `> 0` keeps the current drive's root, which is nothing BUT a
            // separator, from being trimmed away to the empty string.
            match r.char_indices().next_back() {
                Some((i, c)) if i > 0 && style.is_sep(c) => &r[..i],
                _ => r,
            }
        }
        self.same(bare(self, a), bare(self, b))
    }

    /// Whether two segments (or two roots) name the same thing.
    ///
    /// The Windows fold is ASCII-only, matching dart's `Style.windows`
    /// comparison. That is deliberately narrower than the full-Unicode
    /// `to_lowercase()` that `importer::absolute_normalized` applies to a
    /// canonical key, so a path with a non-ASCII uppercase letter does not
    /// relativise and is displayed absolute — which is what dart does with it
    /// too, out of the same mismatch between its `canonicalizePart` and its
    /// comparison.
    fn same(self, a: &str, b: &str) -> bool {
        if self == Style::Posix {
            return a == b;
        }
        let mut x = a.chars();
        let mut y = b.chars();
        loop {
            match (x.next(), y.next()) {
                (None, None) => return true,
                (Some(p), Some(q)) => {
                    let alike = p.eq_ignore_ascii_case(&q) || (self.is_sep(p) && self.is_sep(q));
                    if !alike {
                        return false;
                    }
                }
                _ => return false,
            }
        }
    }
}

/// The segments of the relative path from directory `base` to `target`, or
/// `None` when there is no relative spelling at all: one of the two is not
/// absolute, or their roots differ (`D:\x` from `C:\y`, `\\a\b` from `\\a\c`).
///
/// Callers join with whatever separator their OUTPUT wants — `/` for a
/// source-map URL on every platform, [`Style::sep`] for a diagnostic path —
/// which is why this hands back the pieces rather than a string.
pub(crate) fn relative_parts<'a>(style: Style, base: &str, target: &'a str) -> Option<Vec<&'a str>> {
    if !style.roots_eq(style.root(base)?, style.root(target)?) {
        return None;
    }
    let base: Vec<&str> = style.segments(base).collect();
    let target: Vec<&str> = style.segments(target).collect();
    let common = base
        .iter()
        .copied()
        .zip(target.iter().copied())
        .take_while(|&(a, b)| style.same(a, b))
        .count();
    let mut parts = vec![".."; base.len() - common];
    parts.extend_from_slice(&target[common..]);
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::{relative_parts, Style};

    /// Joined with the style's own separator, which is what a diagnostic path
    /// uses. `None` is "no relative spelling exists".
    fn rel(style: Style, base: &str, target: &str) -> Option<String> {
        relative_parts(style, base, target).map(|p| p.join(style.sep()))
    }

    /// #146, verbatim from the first Windows CI run: `FsImporter`'s canonical
    /// key is lowercased (dart's `p.canonicalize` for `Style.windows`) while
    /// `current_dir()` keeps the filesystem's spelling. Compared literally, the
    /// two match only as far as the drive letter and every loaded file is shown
    /// as an absolute temp path.
    #[test]
    fn windows_relativises_a_lowercased_key_against_a_mixed_case_cwd() {
        assert_eq!(
            rel(
                Style::Windows,
                r"C:\Users\RUNNER~1\AppData\Local\Temp\sasso_frames",
                r"c:\users\runner~1\appdata\local\temp\sasso_frames\src\sub\_warnme.scss",
            )
            .as_deref(),
            Some(r"src\sub\_warnme.scss")
        );
    }

    /// The same spelling difference is a real difference on POSIX, where two
    /// cases are two files.
    #[test]
    fn posix_case_is_significant() {
        assert_eq!(
            rel(Style::Posix, "/Users/me/app", "/users/me/app/src/a.scss").as_deref(),
            Some("../../../users/me/app/src/a.scss")
        );
        assert_eq!(
            rel(Style::Posix, "/Users/me/app", "/Users/me/app/src/a.scss").as_deref(),
            Some("src/a.scss")
        );
    }

    /// Windows reads `/` as a separator, so a path spelled either way splits
    /// and compares the same.
    #[test]
    fn windows_accepts_either_separator() {
        assert_eq!(
            rel(Style::Windows, r"C:\dev\app", "C:/dev/app/src/a.scss").as_deref(),
            Some(r"src\a.scss")
        );
    }

    /// A `..` chain when the target is not under the base, and the mixed case
    /// that produced #146 on the way out of the tree as well as into it.
    #[test]
    fn walks_up_out_of_the_base() {
        assert_eq!(
            rel(Style::Windows, r"C:\Dev\App\out", r"c:\dev\app\src\a.scss").as_deref(),
            Some(r"..\src\a.scss")
        );
        assert_eq!(
            rel(Style::Posix, "/dev/app/out", "/dev/other/a.scss").as_deref(),
            Some("../../other/a.scss")
        );
    }

    /// Different roots have no relative spelling: a `..` chain across drives
    /// would name nothing at all. Callers fall back to the absolute path.
    #[test]
    fn different_roots_have_no_relative_spelling() {
        assert_eq!(rel(Style::Windows, r"C:\dev\app", r"D:\dev\app\src\a.scss"), None);
        assert_eq!(
            rel(Style::Windows, r"\\nas\share\app", r"\\nas\other\app\a.scss"),
            None
        );
        // One side relative: there is no common root to measure from.
        assert_eq!(rel(Style::Windows, "dev", r"C:\dev\a.scss"), None);
        assert_eq!(rel(Style::Posix, "/dev", "dev/a.scss"), None);
    }

    /// A UNC share is the root, not two leading segments, and it folds case
    /// like any other Windows name.
    #[test]
    fn unc_share_is_the_root() {
        assert_eq!(
            rel(Style::Windows, r"\\NAS\Share\app", r"\\nas\share\app\src\a.scss").as_deref(),
            Some(r"src\a.scss")
        );
        assert_eq!(
            rel(
                Style::Windows,
                r"\\nas\share\app\out",
                r"\\nas\share\app\src\a.scss"
            )
            .as_deref(),
            Some(r"..\src\a.scss")
        );
    }

    /// A drive letter without a separator after it is DRIVE-RELATIVE: `C:foo`
    /// means "foo in whatever directory this process is in on C:", which is
    /// not something a relative path can be measured from or to.
    #[test]
    fn a_drive_without_a_separator_is_not_a_root() {
        assert_eq!(rel(Style::Windows, r"C:\dev\app", "C:foo"), None);
        assert_eq!(rel(Style::Windows, "C:foo", r"C:\dev\app\a.scss"), None);
        // Both drive-relative: still nothing to measure, even though the two
        // spellings share a drive letter.
        assert_eq!(rel(Style::Windows, "C:foo", "C:bar"), None);
        // The drive alone is the same case.
        assert_eq!(rel(Style::Windows, "C:", r"C:\a.scss"), None);
    }

    /// A share is spelled `\\server\share` when it is the whole path and
    /// `\\server\share\` when something follows it, so a working directory
    /// that IS the share used to match nothing under it.
    #[test]
    fn a_root_matches_with_or_without_its_trailing_separator() {
        assert_eq!(
            rel(Style::Windows, r"\\nas\share", r"\\nas\share\app\a.scss").as_deref(),
            Some(r"app\a.scss")
        );
        assert_eq!(
            rel(Style::Windows, r"\\nas\share\", r"\\nas\share\app\a.scss").as_deref(),
            Some(r"app\a.scss")
        );
        // A different share still does not match, trailing separator or not.
        assert_eq!(rel(Style::Windows, r"\\nas\share", r"\\nas\other\a.scss"), None);
    }

    /// `std::fs::canonicalize` returns a verbatim path and `current_dir` does
    /// not, so one side of a comparison can carry a `\\?\` the other lacks.
    #[test]
    fn a_verbatim_prefix_is_not_part_of_the_root_identity() {
        assert_eq!(
            rel(Style::Windows, r"\\?\C:\dev\app", r"C:\dev\app\src\a.scss").as_deref(),
            Some(r"src\a.scss")
        );
        assert_eq!(
            rel(Style::Windows, r"C:\dev\app", r"\\?\c:\dev\app\src\a.scss").as_deref(),
            Some(r"src\a.scss")
        );
    }

    /// Repeated separators and a trailing one are not empty segments.
    #[test]
    fn empty_segments_are_dropped() {
        assert_eq!(
            rel(Style::Windows, r"C:\dev\app\", r"C:\dev\\app\src\a.scss").as_deref(),
            Some(r"src\a.scss")
        );
        assert_eq!(
            rel(Style::Posix, "/dev/app/", "/dev/app//src/a.scss").as_deref(),
            Some("src/a.scss")
        );
    }

    /// The root itself: dart counts it as one segment when it splits a path,
    /// which is what `segments` deliberately does not include.
    #[test]
    fn segments_exclude_the_root() {
        assert_eq!(
            Style::Windows.segments(r"C:\dev\app\a.scss").collect::<Vec<_>>(),
            ["dev", "app", "a.scss"]
        );
        assert_eq!(
            Style::Windows.segments(r"\\nas\share\a.scss").collect::<Vec<_>>(),
            ["a.scss"]
        );
        assert_eq!(
            Style::Windows.segments(r"\\?\C:\dev\a.scss").collect::<Vec<_>>(),
            ["dev", "a.scss"]
        );
        assert_eq!(
            Style::Posix.segments("/dev/a.scss").collect::<Vec<_>>(),
            ["dev", "a.scss"]
        );
        assert_eq!(
            Style::Posix.segments("dev/a.scss").collect::<Vec<_>>(),
            ["dev", "a.scss"]
        );
    }

    /// The Windows fold is ASCII-only, as dart's is — and the canonical key it
    /// is compared against was lowercased with full Unicode rules, so a
    /// non-ASCII uppercase letter anywhere in the path defeats relativisation
    /// on both implementations alike.
    #[test]
    fn the_windows_fold_is_ascii_only() {
        assert_eq!(
            rel(Style::Windows, r"C:\dev\ÄRGER", r"c:\dev\ärger\a.scss").as_deref(),
            Some(r"..\ärger\a.scss"),
            "the non-ASCII segment does not fold, so the walk leaves the directory"
        );
    }
}
