//! `%m`: the text of an errno, as PostgreSQL's own `snprintf` prints it.
//!
//! C expands `%m` to `strerror_r(save_errno, …)` (`src/port/snprintf.c:724`),
//! which `src/port/strerror.c:46`'s `pg_strerror_r` stands behind. A Rust
//! `std::io::Error` already carries that text, from the same `strerror`, but
//! its `Display` appends ` (os error N)`; C's `%m` does not.
//!
//! This is its own module rather than a corner of [`crate::validate`] because
//! it is not pre-flight validation: `validate`, `layout` and `sync` all need
//! it, and so does `pgdrop`'s `install` applet, which would otherwise reach
//! into another tool's validation module for a generic errno formatter
//! (NAT-422; NAT-424 records why it stays in this crate rather than moving to
//! a shared one).

/// What `%m` expands to for this error: `strerror(errno)` and nothing else.
///
/// `std::io::Error`'s own `Display` appends ` (os error N)` to the same
/// `strerror` text; C's `%m` does not, so the suffix is removed rather than a
/// second error table being invented.
#[must_use]
pub fn strerror(err: &std::io::Error) -> String {
    let text = err.to_string();
    match err.raw_os_error() {
        Some(code) => text
            .strip_suffix(&format!(" (os error {code})"))
            .unwrap_or(&text)
            .to_owned(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `%m` prints for `ENOENT`.
    const ENOENT: &str = "No such file or directory";

    #[test]
    fn strerror_drops_the_os_error_suffix_rust_adds() {
        let err = std::io::Error::from_raw_os_error(2);
        assert!(err.to_string().contains("(os error 2)"));
        assert_eq!(strerror(&err), ENOENT);
    }

    #[test]
    fn strerror_leaves_an_error_without_an_errno_alone() {
        let err = std::io::Error::other("something else");
        assert_eq!(strerror(&err), "something else");
    }
}
