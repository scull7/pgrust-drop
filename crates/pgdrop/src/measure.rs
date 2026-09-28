//! Startup time and binary size, measured (NAT-410): the pure half of
//! `benches/startup.rs`.
//!
//! The bench times four phases of a throwaway cluster's life, the way a test
//! suite lives it: `pgdrop initdb` expanding the embedded template, the
//! server coming up until `postmaster.pid` says `ready` (what `pg_ctl start`
//! waits for), the first `select 1` through `pgdrop psql`, and a fast
//! shutdown (`pg_ctl stop`'s default `SIGINT`) until the server exits. It
//! reports p50 and p95 of each over N runs, and splits the binary's size
//! between what pgdrop embeds and everything else.
//!
//! It also times the same life through pgdrop's own commands (`start`,
//! `psql`, `stop`), and it gates: each CI lane's dev-profile build has a
//! budget 20% above what that lane measured in CI run 36291661005 (PR #38),
//! on the p50 of the four phases' total and on the binary's size. p95 and
//! the size breakdown are printed beside it, not gated.
//!
//! Everything here is a calculation: parsing the pid file, percentiles,
//! the size breakdown, the budgets and the report text. Spawning,
//! signalling, timing and failing stay in the bench.

use std::fmt::Write as _;
use std::time::Duration;

/// A timed phase of one cluster's life, in the order they happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// `pgdrop initdb`, from spawn to exit.
    Initdb,
    /// `postgres -D …`, from spawn to `postmaster.pid`'s status `ready`.
    Ready,
    /// `pgdrop psql -c 'select 1'` against the ready server, spawn to exit.
    FirstSelect,
    /// `SIGINT` (fast shutdown) to the server's exit.
    Stop,
}

impl Phase {
    /// Every phase, in order.
    pub const ALL: [Phase; 4] = [Phase::Initdb, Phase::Ready, Phase::FirstSelect, Phase::Stop];

    /// The phase's name in the report.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Phase::Initdb => "initdb (template)",
            Phase::Ready => "server ready",
            Phase::FirstSelect => "first select 1",
            Phase::Stop => "stop (fast)",
        }
    }
}

/// One run's time for each phase, indexed as [`Phase::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Run(pub [Duration; 4]);

impl Run {
    /// The time for `phase`.
    #[must_use]
    pub fn get(&self, phase: Phase) -> Duration {
        self.0[phase as usize]
    }

    /// Record the time for `phase`.
    pub fn set(&mut self, phase: Phase, time: Duration) {
        self.0[phase as usize] = time;
    }

    /// All four phases end to end: an empty directory to a stopped cluster
    /// that answered one query.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.0.iter().sum()
    }
}

/// `src/include/utils/pidfile.h:44` — the line of `postmaster.pid` that
/// carries the status, 1-based.
pub const LOCK_FILE_LINE_PM_STATUS: usize = 8;

/// `src/include/utils/pidfile.h:37`.
pub const LOCK_FILE_LINE_PID: usize = 1;

/// Pure: has the postmaster `pid` finished starting, by the contents of its
/// `postmaster.pid`? `src/bin/pg_ctl/pg_ctl.c:606`-`:643`: the file must
/// reach the status line, name this process, and say `ready   ` or
/// `standby ` (`pidfile.h:53`-`:54`, blank-padded to a fixed width). pg_ctl's
/// start-time check (`pg_ctl.c:622`) is left out: the bench removes each cluster, so
/// no stale file can name a live process.
#[must_use]
pub fn postmaster_ready(pidfile: &str, pid: u32) -> bool {
    let lines: Vec<&str> = pidfile.split('\n').collect();
    if lines.len() < LOCK_FILE_LINE_PM_STATUS {
        return false;
    }
    lines[LOCK_FILE_LINE_PID - 1].trim().parse::<u32>() == Ok(pid)
        && matches!(lines[LOCK_FILE_LINE_PM_STATUS - 1], "ready   " | "standby ")
}

/// Pure: the nearest-rank `pct`th percentile of `times`: the smallest time
/// at least `pct` percent of the runs did not exceed. Zero for no runs.
#[must_use]
pub fn percentile(times: &[Duration], pct: u32) -> Duration {
    let mut sorted = times.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    if n == 0 {
        return Duration::ZERO;
    }
    let pct = usize::try_from(pct.min(100)).unwrap_or(100);
    let rank = (pct * n).div_ceil(100).max(1);
    sorted[rank - 1]
}

/// Where the binary's bytes go. Only the embedded assets can be counted from
/// inside the program; `server_and_tools` is the rest of the file — pgrust's
/// server, initdb, psql, and in a debug build the debug info.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeBreakdown {
    /// The executable's size on disk.
    pub binary: u64,
    /// `rinitdb::image`: the template cluster image, its `pg_control` and
    /// its manifest.
    pub template: u64,
    /// `crate::share::FILES`: `timezonesets` and `tsearch_data`.
    pub share: u64,
    /// `rinitdb::conf`: the three configuration samples initdb writes from.
    pub conf_samples: u64,
}

impl SizeBreakdown {
    /// This build's embedded assets, in a binary of `binary` bytes.
    #[must_use]
    pub fn of_this_build(binary: u64) -> Self {
        let len = |bytes: usize| u64::try_from(bytes).unwrap_or(u64::MAX);
        Self {
            binary,
            template: len(rinitdb::image::TEMPLATE.len()
                + rinitdb::image::TEMPLATE_CONTROL.len()
                + rinitdb::image::TEMPLATE_MANIFEST.len()),
            share: len(crate::share::FILES
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum()),
            conf_samples: len(rinitdb::conf::POSTGRESQL_CONF_SAMPLE.len()
                + rinitdb::conf::PG_HBA_CONF_SAMPLE.len()
                + rinitdb::conf::PG_IDENT_CONF_SAMPLE.len()),
        }
    }

    /// Everything that is not an embedded asset.
    #[must_use]
    pub fn server_and_tools(&self) -> u64 {
        self.binary
            .saturating_sub(self.template + self.share + self.conf_samples)
    }
}

fn ms(time: Duration) -> String {
    format!("{:.1} ms", time.as_secs_f64() * 1000.0)
}

fn mib(bytes: u64) -> String {
    // Report precision only; a binary is far below f64's exact-integer range.
    #[allow(clippy::cast_precision_loss)]
    let mib = bytes as f64 / (1024.0 * 1024.0);
    format!("{mib:.2} MiB")
}

/// One report row: `name`, then p50 / p95 of `times`.
fn row(out: &mut String, name: &str, times: &[Duration]) {
    let _ = writeln!(
        out,
        "  {name:<20} {:>10} / {:>10}",
        ms(percentile(times, 50)),
        ms(percentile(times, 95))
    );
}

/// Pure: the startup report, one line per phase plus the total, p50 and
/// p95 over `runs`.
#[must_use]
pub fn render_startup(profile: &str, runs: &[Run]) -> String {
    let mut out = format!(
        "pgdrop startup ({profile} build), {} runs: p50 / p95\n",
        runs.len()
    );
    for phase in Phase::ALL {
        let times: Vec<Duration> = runs.iter().map(|run| run.get(phase)).collect();
        row(&mut out, phase.name(), &times);
    }
    let totals: Vec<Duration> = runs.iter().map(Run::total).collect();
    row(&mut out, "total", &totals);
    out
}

/// One cluster's life through pgdrop's own commands, the way a test suite
/// drives it: `pgdrop start --json`, `pgdrop psql -c 'select 1'`, `pgdrop
/// stop --datadir DIR`, each from spawn to exit. Reported, not budgeted:
/// the baselines the budgets come from (#38) predate `start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Session {
    /// `pgdrop start --json`: mint, spawn, wait for ReadyForQuery.
    pub start: Duration,
    /// `pgdrop psql -c 'select 1'`.
    pub select: Duration,
    /// `pgdrop stop --datadir DIR`: signal, wait, remove.
    pub stop: Duration,
}

impl Session {
    /// All three commands end to end.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.start + self.select + self.stop
    }
}

/// Pure: the session report, one line per command plus the total, p50 and
/// p95 over `sessions`.
#[must_use]
pub fn render_sessions(profile: &str, sessions: &[Session]) -> String {
    let mut out = format!(
        "pgdrop start / psql / stop ({profile} build), {} runs: p50 / p95\n",
        sessions.len()
    );
    let column =
        |pick: fn(&Session) -> Duration| -> Vec<Duration> { sessions.iter().map(pick).collect() };
    row(&mut out, "start --json", &column(|s| s.start));
    row(&mut out, "psql select 1", &column(|s| s.select));
    row(&mut out, "stop --datadir", &column(|s| s.stop));
    row(&mut out, "total", &column(Session::total));
    out
}

/// Pure: the `"datadir"` of `pgdrop start --json`'s one-line object
/// ([`crate::start::StartPlan::json`]). `None` if it is missing, or holds an
/// escape: a scratch path never needs one, so this does not decode them.
#[must_use]
pub fn started_datadir(json: &str) -> Option<&str> {
    let (_, rest) = json.split_once("\"datadir\": \"")?;
    let (value, _) = rest.split_once('"')?;
    (!value.contains('\\')).then_some(value)
}

/// A CI lane (ADR-0007): the target a budget was measured on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// x86_64 Linux, musl (Alpine): the `musl` job.
    Musl,
    /// x86_64 Linux, glibc: the `gnu` job.
    Gnu,
    /// aarch64 macOS: the `apple` job.
    Apple,
}

impl Lane {
    /// The lane this build's target is, if it is one of CI's.
    #[must_use]
    pub fn of_this_build() -> Option<Self> {
        if cfg!(all(
            target_os = "linux",
            target_arch = "x86_64",
            target_env = "musl"
        )) {
            Some(Lane::Musl)
        } else if cfg!(all(
            target_os = "linux",
            target_arch = "x86_64",
            target_env = "gnu"
        )) {
            Some(Lane::Gnu)
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Lane::Apple)
        } else {
            None
        }
    }

    /// The CI job's name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Lane::Musl => "musl",
            Lane::Gnu => "gnu",
            Lane::Apple => "apple",
        }
    }

    /// What this lane's dev-profile build measured in CI run 36291661005
    /// (PR #38, head baf1813, 20 runs): the p50 of the four phases' total,
    /// and the binary's size.
    #[must_use]
    pub fn baseline(self) -> Limits {
        let (p50_micros, binary) = match self {
            Lane::Musl => (583_700, 382_376_208),
            Lane::Gnu => (368_800, 386_856_432),
            Lane::Apple => (413_000, 236_828_680),
        };
        Limits {
            startup_p50: Duration::from_micros(p50_micros),
            binary,
        }
    }
}

/// The CI run the baselines come from, named in the budget report.
pub const BASELINE_RUN: &str = "36291661005";

/// A startup time and a binary size: a lane's baseline, its budget, or what
/// this build measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// p50 of [`Run::total`]: an empty directory to a stopped cluster.
    pub startup_p50: Duration,
    /// The executable's size on disk.
    pub binary: u64,
}

impl Limits {
    /// Pure: the budget 20% above this baseline (NAT-410), rounded down.
    #[must_use]
    pub fn budget(self) -> Self {
        Self {
            startup_p50: self.startup_p50 * 6 / 5,
            binary: self.binary * 6 / 5,
        }
    }
}

/// Pure: the budget for a `debug` build on `lane`. Only the dev profile has
/// one, because only it has CI baselines; release numbers are NAT-411's.
#[must_use]
pub fn budget_for(lane: Option<Lane>, debug: bool) -> Option<Limits> {
    lane.filter(|_| debug).map(|lane| lane.baseline().budget())
}

/// Pure: the budget report, and whether `measured` is within `budget`: one
/// line per limit, each `ok` or `OVER`.
#[must_use]
pub fn render_budget(
    profile: &str,
    lane: Lane,
    budget: &Limits,
    measured: &Limits,
) -> (String, bool) {
    let mut out = format!(
        "pgdrop budget ({profile} build, {} lane; 20% above CI run {BASELINE_RUN})\n",
        lane.name()
    );
    let verdict = |within: bool| if within { "ok" } else { "OVER" };
    let time_ok = measured.startup_p50 <= budget.startup_p50;
    let size_ok = measured.binary <= budget.binary;
    let _ = writeln!(
        out,
        "  {:<20} {:>12} <= {:>12}  {}",
        "startup p50 (total)",
        ms(measured.startup_p50),
        ms(budget.startup_p50),
        verdict(time_ok)
    );
    let _ = writeln!(
        out,
        "  {:<20} {:>12} <= {:>12}  {}",
        "binary size",
        format!("{} B", measured.binary),
        format!("{} B", budget.binary),
        verdict(size_ok)
    );
    (out, time_ok && size_ok)
}

/// Pure: the size report, one line per part, bytes and MiB.
#[must_use]
pub fn render_size(profile: &str, size: &SizeBreakdown) -> String {
    let mut out = format!("pgdrop binary size ({profile} build)\n");
    for (name, bytes) in [
        ("template image", size.template),
        ("share files", size.share),
        ("conf samples", size.conf_samples),
        ("server and tools", size.server_and_tools()),
        ("total", size.binary),
    ] {
        let _ = writeln!(out, "  {name:<20} {bytes:>12} B {:>12}", mib(bytes));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms_list(list: &[u64]) -> Vec<Duration> {
        list.iter().copied().map(Duration::from_millis).collect()
    }

    #[test]
    fn a_ready_pidfile_names_the_pid_and_says_ready_on_line_8() {
        let ready = "4242\n/d\n1790479677\n5432\n/s\n\n\nready   \n";
        assert!(postmaster_ready(ready, 4242));
        assert!(!postmaster_ready(ready, 4243));
        assert!(postmaster_ready(
            &ready.replace("ready   ", "standby "),
            4242
        ));
        assert!(!postmaster_ready(
            &ready.replace("ready   ", "starting"),
            4242
        ));
        assert!(!postmaster_ready(
            &ready.replace("ready   ", "stopping"),
            4242
        ));
        // Still being written: the status line is not there yet.
        assert!(!postmaster_ready("4242\n/d\n1790479677\n5432\n/s\n", 4242));
        assert!(!postmaster_ready("", 4242));
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        let twenty: Vec<u64> = (1..=20).rev().collect();
        let times = ms_list(&twenty);
        assert_eq!(percentile(&times, 50), Duration::from_millis(10));
        assert_eq!(percentile(&times, 95), Duration::from_millis(19));
        assert_eq!(percentile(&times, 100), Duration::from_millis(20));
        assert_eq!(percentile(&times, 0), Duration::from_millis(1));
        assert_eq!(percentile(&ms_list(&[7]), 95), Duration::from_millis(7));
        assert_eq!(percentile(&[], 50), Duration::ZERO);
    }

    #[test]
    fn a_run_totals_its_phases() {
        let mut run = Run::default();
        for (phase, millis) in Phase::ALL.into_iter().zip([1, 2, 3, 4]) {
            run.set(phase, Duration::from_millis(millis));
        }
        assert_eq!(run.get(Phase::FirstSelect), Duration::from_millis(3));
        assert_eq!(run.total(), Duration::from_millis(10));
    }

    #[test]
    fn the_size_breakdown_counts_every_embedded_asset() {
        let size = SizeBreakdown::of_this_build(100 << 20);
        assert!(size.template > u64::try_from(rinitdb::image::TEMPLATE.len()).unwrap());
        assert!(size.share > 0 && size.conf_samples > 0);
        assert_eq!(
            size.server_and_tools() + size.template + size.share + size.conf_samples,
            size.binary
        );
        // A binary cannot be smaller than what it embeds; if a caller says
        // so, the rest is zero rather than an underflow.
        assert_eq!(SizeBreakdown::of_this_build(0).server_and_tools(), 0);
    }

    #[test]
    fn budgets_are_20_percent_above_the_ci_baselines() {
        let gnu = Lane::Gnu.baseline().budget();
        assert_eq!(gnu.startup_p50, Duration::from_micros(442_560));
        assert_eq!(gnu.binary, 464_227_718);
        let musl = Lane::Musl.baseline().budget();
        assert_eq!(musl.startup_p50, Duration::from_micros(700_440));
        assert_eq!(musl.binary, 458_851_449);
        let apple = Lane::Apple.baseline().budget();
        assert_eq!(apple.startup_p50, Duration::from_micros(495_600));
        assert_eq!(apple.binary, 284_194_416);
    }

    #[test]
    fn only_a_ci_lanes_dev_build_has_a_budget() {
        assert_eq!(
            budget_for(Some(Lane::Apple), true),
            Some(Lane::Apple.baseline().budget())
        );
        assert_eq!(budget_for(Some(Lane::Apple), false), None);
        assert_eq!(budget_for(None, true), None);
    }

    #[test]
    fn the_budget_report_says_ok_or_over_for_each_limit() {
        let budget = Limits {
            startup_p50: Duration::from_millis(400),
            binary: 1000,
        };
        let within = Limits {
            startup_p50: Duration::from_micros(399_950),
            binary: 1000,
        };
        let (text, ok) = render_budget("debug", Lane::Gnu, &budget, &within);
        assert!(ok);
        assert_eq!(
            text,
            "pgdrop budget (debug build, gnu lane; 20% above CI run 36291661005)\n\
             \x20 startup p50 (total)      400.0 ms <=     400.0 ms  ok\n\
             \x20 binary size                1000 B <=       1000 B  ok\n"
        );

        let slow = Limits {
            startup_p50: Duration::from_millis(401),
            ..within
        };
        let (text, ok) = render_budget("debug", Lane::Gnu, &budget, &slow);
        assert!(!ok);
        assert!(text.contains("401.0 ms <=     400.0 ms  OVER\n"), "{text}");
        assert!(text.contains("1000 B  ok\n"), "{text}");

        let big = Limits {
            binary: 1001,
            ..within
        };
        let (text, ok) = render_budget("debug", Lane::Gnu, &budget, &big);
        assert!(!ok);
        assert!(text.contains("1001 B <=       1000 B  OVER\n"), "{text}");
    }

    #[test]
    fn the_datadir_is_read_from_starts_json() {
        let json = "{\"uri\": \"postgresql://postgres@%2Ftmp%2Fpgdrop-1-2:5432/postgres\", \
                    \"pid\": 7, \"datadir\": \"/tmp/pgdrop-1-2/data\"}\n";
        assert_eq!(started_datadir(json), Some("/tmp/pgdrop-1-2/data"));
        assert_eq!(started_datadir("{\"datadir\": \"/a\\\"b\"}"), None);
        assert_eq!(started_datadir("{\"pid\": 7}"), None);
    }

    #[test]
    fn the_session_report_names_every_command() {
        let session = Session {
            start: Duration::from_millis(3),
            select: Duration::from_millis(2),
            stop: Duration::from_millis(1),
        };
        assert_eq!(session.total(), Duration::from_millis(6));
        let text = render_sessions("debug", &[session]);
        assert!(text.starts_with("pgdrop start / psql / stop (debug build), 1 runs: p50 / p95\n"));
        for name in ["start --json", "psql select 1", "stop --datadir"] {
            assert!(text.contains(name), "{text}");
        }
        assert!(
            text.contains("  total                    6.0 ms /     6.0 ms\n"),
            "{text}"
        );
    }

    #[test]
    fn the_reports_name_every_phase_and_part() {
        let run = Run([Duration::from_micros(1500); 4]);
        let text = render_startup("debug", &[run, run]);
        assert!(text.starts_with("pgdrop startup (debug build), 2 runs: p50 / p95\n"));
        for phase in Phase::ALL {
            assert!(text.contains(phase.name()), "{text}");
        }
        assert!(
            text.contains("  total                    6.0 ms /     6.0 ms\n"),
            "{text}"
        );

        let size = SizeBreakdown {
            binary: 3 << 20,
            template: 1 << 20,
            share: 1 << 19,
            conf_samples: 0,
        };
        let text = render_size("release", &size);
        assert_eq!(
            text,
            "pgdrop binary size (release build)\n\
             \x20 template image            1048576 B     1.00 MiB\n\
             \x20 share files                524288 B     0.50 MiB\n\
             \x20 conf samples                    0 B     0.00 MiB\n\
             \x20 server and tools          1572864 B     1.50 MiB\n\
             \x20 total                     3145728 B     3.00 MiB\n"
        );
    }
}
