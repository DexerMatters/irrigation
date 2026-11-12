//! Unit tests for `src/http.rs`.

use super::*;

#[test]
fn a_plain_target_resolves_inside_the_directory() {
    let dir = Path::new("/srv/observatory");
    assert_eq!(
        static_candidate(dir, "assets/app.js"),
        Some(PathBuf::from("/srv/observatory/assets/app.js"))
    );
    assert_eq!(
        static_candidate(dir, "index.html"),
        Some(PathBuf::from("/srv/observatory/index.html"))
    );
}

#[test]
fn a_target_that_escapes_the_directory_is_refused() {
    let dir = Path::new("/srv/observatory");
    assert_eq!(static_candidate(dir, "../etc/passwd"), None, "climbs out");
    assert_eq!(static_candidate(dir, "a/../../b"), None, "climbs out");
    assert_eq!(static_candidate(dir, "/etc/passwd"), None, "absolute");
}

/// A volume is refused where the platform has one.
///
/// `Path::join` replaces the base when its argument names a volume, so on Windows `C:/x` would
/// read outside the directory -- the case the `Prefix` arm exists for. On Unix there is no prefix
/// component, so the same target resolves *inside* the directory, which is harmless; the test
/// asserts the property each platform actually has rather than pretending they are the same.
#[test]
fn a_volume_target_cannot_escape_the_directory() {
    let dir = Path::new("/srv/observatory");
    for target in [
        "C:/Windows/win.ini",
        "C:\\Windows\\win.ini",
        "\\\\server\\share\\x",
    ] {
        // Refused outright where the platform has volume prefixes; elsewhere the target is an
        // ordinary relative path and must at least stay inside the directory.
        if let Some(resolved) = static_candidate(dir, target) {
            assert!(
                resolved.starts_with(dir),
                "{target} must not leave the directory: {resolved:?}"
            );
        }
    }
}
