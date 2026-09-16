//! The environment a spawned command runs in.
//!
//! `PostgreSQL::Test::Utils`' `BEGIN` block
//! (`src/test/perl/PostgreSQL/Test/Utils.pm:105`) deletes every `PG*` variable
//! that could change a client's behaviour, pins `LC_MESSAGES` to `C` so the
//! messages are the untranslated ones the expectations were written against,
//! and sets `PGAPPNAME` to the script's name. A test that skips that reads the
//! developer's own `PGHOST`/`PGPORT` and then passes or fails by accident, so
//! the scrubbing is data here rather than a habit.
//!
//! Data / Calculations / Actions: [`Environment`] is the data, [`Environment::apply`]
//! is the only action.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::process::Command;

/// The variables `Utils.pm:116` deletes, in its order.
///
/// Copied verbatim, including what it leaves alone: `PGOPTIONS`, `PGAPPNAME`,
/// `PGSSLNEGOTIATION`, `PGSSLCERTMODE`, `PGREQUIREAUTH`, `PGLOADBALANCEHOSTS`,
/// `PGMINPROTOCOLVERSION` and `PGMAXPROTOCOLVERSION` are *not* on upstream's
/// list even though libpq reads them. Adding them here would make our stolen
/// tests pass in an environment where upstream's fail, which is the wrong
/// direction to diverge in; upstream's comment on the list is "This list should
/// be kept in sync with pg_regress.c."
pub const PG_ENV_KEYS: [&str; 30] = [
    "PGCHANNELBINDING",
    "PGCLIENTENCODING",
    "PGCONNECT_TIMEOUT",
    "PGDATA",
    "PGDATABASE",
    "PGGSSDELEGATION",
    "PGGSSENCMODE",
    "PGGSSLIB",
    "PGHOSTADDR",
    "PGKRBSRVNAME",
    "PGPASSFILE",
    "PGPASSWORD",
    "PGREQUIREPEER",
    "PGREQUIRESSL",
    "PGSERVICE",
    "PGSERVICEFILE",
    "PGSSLCERT",
    "PGSSLCRL",
    "PGSSLCRLDIR",
    "PGSSLKEY",
    "PGSSLMAXPROTOCOLVERSION",
    "PGSSLMINPROTOCOLVERSION",
    "PGSSLMODE",
    "PGSSLROOTCERT",
    "PGSSLSNI",
    "PGTARGETSESSIONATTRS",
    "PGUSER",
    "PGPORT",
    "PGHOST",
    "PG_COLOR",
];

/// What to change about the environment a child inherits.
///
/// Removals are applied before assignments, so `postgres_test` can delete a
/// variable the caller then sets — which is exactly what a `001_uri.pl` case
/// with `PGSSLROOTCERT => "system"` needs after the scrub.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// Variables removed from the inherited environment.
    pub removed: Vec<OsString>,
    /// Variables set afterwards, in order.
    pub assigned: Vec<(OsString, OsString)>,
}

impl Environment {
    /// Inherit the caller's environment unchanged.
    #[must_use]
    pub fn inherited() -> Self {
        Self::default()
    }

    /// `Utils.pm:105`'s `BEGIN` block: the scrub every TAP test starts from.
    ///
    /// `app_name` is upstream's `basename($0)` (`Utils.pm:150`) — the name of
    /// the `.pl` file this test is a port of, so `application_name`'s default
    /// is the same string it would be over there.
    #[must_use]
    pub fn postgres_test(app_name: &str) -> Self {
        Self::inherited()
            .without("LANGUAGE")
            .without("LC_ALL")
            .without_all(PG_ENV_KEYS)
            .with("LC_MESSAGES", "C")
            .with("PGAPPNAME", app_name)
    }

    /// Remove one variable.
    #[must_use]
    pub fn without(mut self, key: impl Into<OsString>) -> Self {
        self.removed.push(key.into());
        self
    }

    /// Remove several variables.
    #[must_use]
    pub fn without_all<I, S>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.removed
            .extend(keys.into_iter().map(|key| key.as_ref().to_owned()));
        self
    }

    /// Set one variable, after the removals.
    #[must_use]
    pub fn with(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.assigned.push((key.into(), value.into()));
        self
    }

    /// Nothing to change: the child inherits this process's environment.
    #[must_use]
    pub fn is_inherited(&self) -> bool {
        self.removed.is_empty() && self.assigned.is_empty()
    }

    /// Action: apply this plan to a command that has not been spawned yet.
    pub fn apply(&self, command: &mut Command) {
        for key in &self.removed {
            command.env_remove(key);
        }
        for (key, value) in &self.assigned {
            command.env(key, value);
        }
    }
}

impl fmt::Display for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for key in &self.removed {
            let separator = if first { "" } else { " " };
            write!(f, "{separator}-{}", key.to_string_lossy())?;
            first = false;
        }
        for (key, value) in &self.assigned {
            let separator = if first { "" } else { " " };
            write!(
                f,
                "{separator}{}={}",
                key.to_string_lossy(),
                value.to_string_lossy()
            )?;
            first = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inherited_environment_changes_nothing() {
        assert!(Environment::inherited().is_inherited());
    }

    #[test]
    fn the_scrub_deletes_every_key_utils_pm_deletes() {
        let plan = Environment::postgres_test("001_uri.pl");
        for key in PG_ENV_KEYS {
            assert!(
                plan.removed.iter().any(|removed| removed == key),
                "{key} is on Utils.pm's list but not in the plan"
            );
        }
    }

    #[test]
    fn the_scrub_sets_the_two_variables_utils_pm_sets() {
        let plan = Environment::postgres_test("001_uri.pl");
        assert_eq!(
            plan.assigned,
            vec![
                (OsString::from("LC_MESSAGES"), OsString::from("C")),
                (OsString::from("PGAPPNAME"), OsString::from("001_uri.pl")),
            ]
        );
    }

    #[test]
    fn a_later_assignment_survives_the_scrub_that_removed_it() {
        // `PGSSLROOTCERT => "system"` in a 001_uri.pl case, applied on top of
        // the scrub that deletes PGSSLROOTCERT.
        let plan = Environment::postgres_test("001_uri.pl").with("PGSSLROOTCERT", "system");
        assert!(plan.removed.iter().any(|key| key == "PGSSLROOTCERT"));
        assert_eq!(
            plan.assigned.last(),
            Some(&(OsString::from("PGSSLROOTCERT"), OsString::from("system")))
        );
    }

    #[test]
    fn display_names_removals_and_assignments() {
        let plan = Environment::inherited()
            .without("PGHOST")
            .with("PGPORT", "1");
        assert_eq!(plan.to_string(), "-PGHOST PGPORT=1");
    }
}
