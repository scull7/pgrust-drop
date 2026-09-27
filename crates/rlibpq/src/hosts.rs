//! The host list: which servers a connection string names, in what order a
//! connection tries them, and what kind of server it will settle for.
//!
//! Ported from `src/interfaces/libpq/fe-connect.c`: the host, hostaddr and
//! port splitting at the top of `pqConnectOptions2` (`:1247`-`:1392`, over
//! `count_comma_separated_elems`, `:1110`, and `parse_comma_separated_list`,
//! `:1134`), the `target_session_attrs` and `load_balance_hosts` checks
//! (`:1990`-`:2083`), the shuffle that `load_balance_hosts=random` applies
//! to the hosts (`:2085`-`:2104`) and, per host, to its addresses
//! (`:3116`-`:3136`), and `libpq_prng_init` (`:1169`) over
//! `src/common/pg_prng.c`, the generator that shuffle draws from.
//!
//! Everything here is pure; `Connection::connect` walks the list.

use crate::connection::is_unixsock_path;
use crate::conninfo::ConnInfo;
use crate::error::ConnError;
use crate::pg_config::DEFAULT_PGSOCKET_DIR;
use crate::text::RawText;

/// `pg_conn_host_type`, `libpq-int.h`: how one host entry is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostType {
    /// `CHT_HOST_NAME`: a name to resolve.
    HostName,
    /// `CHT_HOST_ADDRESS`: a numeric `hostaddr`, used without a lookup.
    HostAddress,
    /// `CHT_UNIX_SOCKET`: a socket directory.
    UnixSocket,
}

/// `pg_conn_host`, `libpq-int.h`: one entry of `conn->connhost[]`.
///
/// `port` is the raw element, possibly empty: like C, the list keeps it
/// unparsed and `PQconnectPoll` reads it only when it reaches this host
/// (`fe-connect.c:3036`), so a bad port on a later host does not stop an
/// earlier one from being tried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnHost {
    pub kind: HostType,
    /// The `host` element; the default location when none was given.
    pub host: Option<Vec<u8>>,
    /// The `hostaddr` element, when `hostaddr` was given.
    pub hostaddr: Option<Vec<u8>>,
    pub port: Option<Vec<u8>>,
}

/// `count_comma_separated_elems`, `fe-connect.c:1110`.
fn count_comma_separated_elems(input: &[u8]) -> usize {
    input.split(|byte| *byte == b',').count()
}

/// The elements `parse_comma_separated_list` (`fe-connect.c:1134`) hands out
/// one call at a time, all at once.
fn comma_separated(input: &[u8]) -> impl Iterator<Item = Vec<u8>> + '_ {
    input.split(|byte| *byte == b',').map(<[u8]>::to_vec)
}

/// `conn->connhost[]` as `pqConnectOptions2` fills it
/// (`fe-connect.c:1256`-`:1392`), before any shuffle.
///
/// The entries follow `hostaddr` when it is given and non-empty, else
/// `host`, else there is one entry for the default location. A `host` list
/// must then match `hostaddr` element for element, and a `port` list must
/// have one element, which every host shares, or one per host.
///
/// # Errors
/// [`ConnError::HostCountMismatch`] and [`ConnError::PortCountMismatch`],
/// `fe-connect.c:1309` and `:1389`.
pub fn conn_hosts(conninfo: &ConnInfo) -> Result<Vec<ConnHost>, ConnError> {
    let given = |keyword| conninfo.get(keyword).filter(|value| !value.is_empty());
    let hostaddr = given("hostaddr");
    let host = given("host");
    let port = given("port");

    // fe-connect.c:1257
    let nconnhost = hostaddr.or(host).map_or(1, count_comma_separated_elems);

    // fe-connect.c:1272 — `hostaddr` sized the array, so it always fits.
    let hostaddrs: Vec<Option<Vec<u8>>> = match hostaddr {
        Some(list) => comma_separated(list).map(Some).collect(),
        None => vec![None; nconnhost],
    };

    // fe-connect.c:1293
    let hosts: Vec<Option<Vec<u8>>> = match host {
        Some(list) => {
            let hosts: Vec<_> = comma_separated(list).map(Some).collect();
            if hosts.len() != nconnhost {
                return Err(ConnError::HostCountMismatch {
                    hosts: hosts.len(),
                    hostaddrs: nconnhost,
                });
            }
            hosts
        }
        None => vec![None; nconnhost],
    };

    // fe-connect.c:1361 — one port serves every host (`:1377`); otherwise
    // there is one per host (`:1386`).
    let ports: Vec<Option<Vec<u8>>> = match port {
        Some(list) => {
            let ports: Vec<_> = comma_separated(list).collect();
            match ports.len() {
                1 => vec![Some(ports[0].clone()); nconnhost],
                n if n == nconnhost => ports.into_iter().map(Some).collect(),
                n => {
                    return Err(ConnError::PortCountMismatch {
                        ports: n,
                        hosts: nconnhost,
                    });
                }
            }
        }
        None => vec![None; nconnhost],
    };

    // fe-connect.c:1319 — classify each slot, filling in the default
    // location where nothing was given.
    Ok(hostaddrs
        .into_iter()
        .zip(hosts)
        .zip(ports)
        .map(|((hostaddr, host), port)| {
            let present = |value: &Option<Vec<u8>>| value.as_ref().is_some_and(|v| !v.is_empty());
            let (kind, host) = if present(&hostaddr) {
                (HostType::HostAddress, host)
            } else if present(&host) {
                let kind = if host.as_deref().is_some_and(is_unixsock_path) {
                    HostType::UnixSocket
                } else {
                    HostType::HostName
                };
                (kind, host)
            } else if DEFAULT_PGSOCKET_DIR.is_empty() {
                // fe-connect.c:1346, `DefaultHost`.
                (HostType::HostName, Some(b"localhost".to_vec()))
            } else {
                (
                    HostType::UnixSocket,
                    Some(DEFAULT_PGSOCKET_DIR.as_bytes().to_vec()),
                )
            };
            ConnHost {
                kind,
                host,
                hostaddr,
                port,
            }
        })
        .collect())
}

/// `target_server_type`, `libpq-int.h`: what `target_session_attrs` asks
/// the server to be. [`crate::target::check_target`] holds a server to it.
///
/// `SERVER_TYPE_PREFER_STANDBY_PASS2` is not a value here: it is the second
/// pass `Connection::connect` makes for `PreferStandby`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetServerType {
    /// `SERVER_TYPE_ANY`.
    Any,
    /// `SERVER_TYPE_READ_WRITE`.
    ReadWrite,
    /// `SERVER_TYPE_READ_ONLY`.
    ReadOnly,
    /// `SERVER_TYPE_PRIMARY`.
    Primary,
    /// `SERVER_TYPE_STANDBY`.
    Standby,
    /// `SERVER_TYPE_PREFER_STANDBY`.
    PreferStandby,
}

impl TargetServerType {
    /// `fe-connect.c:1992`-`:2016`. An absent value is `any`; a present one,
    /// empty included, must be one of the six spellings.
    ///
    /// # Errors
    /// [`ConnError::InvalidValue`] for any other value (`:2009`).
    pub fn from_conninfo(conninfo: &ConnInfo) -> Result<Self, ConnError> {
        let Some(value) = conninfo.get("target_session_attrs") else {
            return Ok(TargetServerType::Any);
        };
        Ok(match value {
            b"any" => TargetServerType::Any,
            b"read-write" => TargetServerType::ReadWrite,
            b"read-only" => TargetServerType::ReadOnly,
            b"primary" => TargetServerType::Primary,
            b"standby" => TargetServerType::Standby,
            b"prefer-standby" => TargetServerType::PreferStandby,
            _ => return Err(invalid("target_session_attrs", value)),
        })
    }
}

/// `load_balance_type`, `libpq-int.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadBalance {
    /// `LOAD_BALANCE_DISABLE`: hosts and addresses in the order given.
    Disable,
    /// `LOAD_BALANCE_RANDOM`: both shuffled.
    Random,
}

impl LoadBalance {
    /// `fe-connect.c:2067`-`:2083`. An absent value is `disable`.
    ///
    /// # Errors
    /// [`ConnError::InvalidValue`] for anything but `disable` or `random`
    /// (`:2076`).
    pub fn from_conninfo(conninfo: &ConnInfo) -> Result<Self, ConnError> {
        match conninfo.get("load_balance_hosts") {
            None | Some(b"disable") => Ok(LoadBalance::Disable),
            Some(b"random") => Ok(LoadBalance::Random),
            Some(value) => Err(invalid("load_balance_hosts", value)),
        }
    }
}

fn invalid(option: &'static str, value: &[u8]) -> ConnError {
    ConnError::InvalidValue {
        option,
        value: RawText::new(value.to_vec()),
    }
}

/// `pg_prng_state`, `src/common/pg_prng.c`: Blackman and Vigna's
/// xoroshiro128** 1.0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prng {
    s0: u64,
    s1: u64,
}

impl Prng {
    /// A state from two words, with `pg_prng_seed_check`'s fix-up
    /// (`pg_prng.c:114`): all-zeroes is a fixed point, so it becomes Knuth's
    /// LCG parameters instead.
    #[must_use]
    pub fn from_state(s0: u64, s1: u64) -> Self {
        if s0 == 0 && s1 == 0 {
            return Prng {
                s0: 0x5851_F42D_4C95_7F2D,
                s1: 0x1405_7B7E_F767_814F,
            };
        }
        Prng { s0, s1 }
    }

    /// `pg_prng_seed`, `pg_prng.c:89`: both words from `splitmix64`.
    #[must_use]
    pub fn seed(seed: u64) -> Self {
        let mut state = seed;
        let s0 = splitmix64(&mut state);
        let s1 = splitmix64(&mut state);
        Prng::from_state(s0, s1)
    }

    /// `pg_prng_strong_seed` (`pg_prng.h:46`) over the sixteen bytes of the
    /// state, which is how `libpq_prng_init` (`fe-connect.c:1169`) seeds
    /// when it can. `bytes` is what `pg_strong_random` drew, read as the
    /// two native-endian words C's `memcpy`-shaped fill leaves.
    #[must_use]
    pub fn strong_seed(bytes: [u8; 16]) -> Self {
        let mut s0 = [0; 8];
        let mut s1 = [0; 8];
        s0.copy_from_slice(&bytes[..8]);
        s1.copy_from_slice(&bytes[8..]);
        Prng::from_state(u64::from_ne_bytes(s0), u64::from_ne_bytes(s1))
    }

    /// `xoroshiro128ss`, `pg_prng.c:54`; `pg_prng_uint64`.
    pub fn next_u64(&mut self) -> u64 {
        let s0 = self.s0;
        let sx = self.s1 ^ s0;
        let val = s0.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        self.s0 = s0.rotate_left(24) ^ sx ^ (sx << 16);
        self.s1 = sx.rotate_left(37);
        val
    }

    /// `pg_prng_uint64_range`, `pg_prng.c:144`: uniform in `[rmin, rmax]`
    /// by bitmask rejection; `rmin` when the range is empty.
    pub fn uint64_range(&mut self, rmin: u64, rmax: u64) -> u64 {
        if rmax <= rmin {
            return rmin;
        }
        let range = rmax - rmin;
        // `63 - pg_leftmost_one_pos64(range)`.
        let rshift = range.leading_zeros();
        loop {
            let val = self.next_u64() >> rshift;
            if val <= range {
                return rmin + val;
            }
        }
    }

    /// The "inside-out" Fisher-Yates shuffle `load_balance_hosts=random`
    /// applies to `conn->connhost[]` (`fe-connect.c:2097`) and to each
    /// host's addresses (`:3129`), one draw per element after the first.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in 1..items.len() {
            let j = self.uint64_range(0, i as u64);
            // `j <= i`, so it always fits; `i` is the unreachable fallback.
            items.swap(usize::try_from(j).unwrap_or(i), i);
        }
    }
}

/// `splitmix64`, `pg_prng.c:72`.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut val = *state;
    val = (val ^ (val >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    val = (val ^ (val >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    val ^ (val >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conninfo::parse_conninfo;

    fn conninfo(s: &str) -> ConnInfo {
        parse_conninfo(s.as_bytes()).unwrap()
    }

    fn host(kind: HostType, host: &str, hostaddr: Option<&str>, port: Option<&str>) -> ConnHost {
        ConnHost {
            kind,
            host: Some(host.as_bytes().to_vec()),
            hostaddr: hostaddr.map(|v| v.as_bytes().to_vec()),
            port: port.map(|v| v.as_bytes().to_vec()),
        }
    }

    #[test]
    fn a_host_list_is_one_entry_per_element_with_its_own_port() {
        assert_eq!(
            conn_hosts(&conninfo("host=/a,/b,db.example port=1,2,3")).unwrap(),
            vec![
                host(HostType::UnixSocket, "/a", None, Some("1")),
                host(HostType::UnixSocket, "/b", None, Some("2")),
                host(HostType::HostName, "db.example", None, Some("3")),
            ]
        );
    }

    #[test]
    fn one_port_serves_every_host() {
        let hosts = conn_hosts(&conninfo("host=a,b,c port=6543")).unwrap();
        assert_eq!(hosts.len(), 3);
        assert!(
            hosts
                .iter()
                .all(|h| h.port.as_deref() == Some(&b"6543"[..]))
        );
    }

    /// fe-connect.c:1341 — an empty element gets the default location, and
    /// an empty port element stays empty for `PQconnectPoll` to default.
    #[test]
    fn an_empty_element_is_the_default_location() {
        let hosts = conn_hosts(&conninfo("host=a, port=1,")).unwrap();
        assert_eq!(hosts[0], host(HostType::HostName, "a", None, Some("1")));
        assert_eq!(
            hosts[1],
            host(HostType::UnixSocket, DEFAULT_PGSOCKET_DIR, None, Some(""))
        );
        assert_eq!(
            conn_hosts(&conninfo("")).unwrap(),
            vec![host(HostType::UnixSocket, DEFAULT_PGSOCKET_DIR, None, None)]
        );
    }

    /// fe-connect.c:1257 — `hostaddr` sizes the list and wins the slot's
    /// type; `host` rides along for its name.
    #[test]
    fn hostaddr_sizes_the_list_and_host_must_match_it() {
        assert_eq!(
            conn_hosts(&conninfo("hostaddr=127.0.0.1,127.0.0.2 host=a,b")).unwrap(),
            vec![
                host(HostType::HostAddress, "a", Some("127.0.0.1"), None),
                host(HostType::HostAddress, "b", Some("127.0.0.2"), None),
            ]
        );
        let mixed = conn_hosts(&conninfo("hostaddr=127.0.0.1, host=a,/tmp")).unwrap();
        assert_eq!(mixed[1].kind, HostType::UnixSocket);
        assert_eq!(
            conn_hosts(&conninfo("hostaddr=127.0.0.1,127.0.0.2 host=a,b,c"))
                .unwrap_err()
                .to_string(),
            "could not match 3 host names to 2 hostaddr values"
        );
    }

    #[test]
    fn a_port_list_must_match_the_hosts() {
        assert_eq!(
            conn_hosts(&conninfo("host=a,b,c port=1,2"))
                .unwrap_err()
                .to_string(),
            "could not match 2 port numbers to 3 hosts"
        );
        assert_eq!(
            conn_hosts(&conninfo("host=a port=1,2"))
                .unwrap_err()
                .to_string(),
            "could not match 2 port numbers to 1 hosts"
        );
    }

    #[test]
    fn target_session_attrs_takes_the_six_spellings_upstream_takes() {
        for (value, expected) in [
            ("any", TargetServerType::Any),
            ("read-write", TargetServerType::ReadWrite),
            ("read-only", TargetServerType::ReadOnly),
            ("primary", TargetServerType::Primary),
            ("standby", TargetServerType::Standby),
            ("prefer-standby", TargetServerType::PreferStandby),
        ] {
            let info = conninfo(&format!("target_session_attrs={value}"));
            assert_eq!(TargetServerType::from_conninfo(&info), Ok(expected));
        }
        assert_eq!(
            TargetServerType::from_conninfo(&conninfo("")),
            Ok(TargetServerType::Any)
        );
        assert_eq!(
            TargetServerType::from_conninfo(&conninfo("target_session_attrs=master"))
                .unwrap_err()
                .to_string(),
            "invalid target_session_attrs value: \"master\""
        );
    }

    /// The message `003_load_balance_host_list.pl:31` expects.
    #[test]
    fn load_balance_hosts_takes_disable_or_random() {
        assert_eq!(
            LoadBalance::from_conninfo(&conninfo("")),
            Ok(LoadBalance::Disable)
        );
        assert_eq!(
            LoadBalance::from_conninfo(&conninfo("load_balance_hosts=random")),
            Ok(LoadBalance::Random)
        );
        assert_eq!(
            LoadBalance::from_conninfo(&conninfo("load_balance_hosts=doesnotexist"))
                .unwrap_err()
                .to_string(),
            "invalid load_balance_hosts value: \"doesnotexist\""
        );
    }

    /// Every number below is what REL_18_6's own `src/common/pg_prng.c`,
    /// compiled and called the same way, printed.
    #[test]
    fn the_generator_draws_what_pg_prng_draws() {
        let mut prng = Prng::seed(0);
        assert_eq!(
            prng,
            Prng {
                s0: 16_294_208_416_658_607_535,
                s1: 7_960_286_522_194_355_700,
            }
        );
        assert_eq!(prng.next_u64(), 16_053_376_993_090_331_485);
        assert_eq!(prng.next_u64(), 7_868_822_567_099_391_496);
        assert_eq!(prng.next_u64(), 12_331_295_923_365_717_130);

        let mut prng = Prng::seed(42);
        let draws: Vec<u64> = (0..5).map(|_| prng.uint64_range(0, 9)).collect();
        assert_eq!(draws, [6, 3, 3, 1, 0]);

        let mut prng = Prng::seed(7);
        assert_eq!(prng.uint64_range(3, 5), 4);
        assert_eq!(prng.uint64_range(5, 5), 5);
    }

    #[test]
    fn an_all_zero_state_is_replaced() {
        assert_ne!(Prng::from_state(0, 0), Prng { s0: 0, s1: 0 });
        assert_eq!(Prng::strong_seed([0; 16]), Prng::from_state(0, 0));
    }

    /// `fe-connect.c:2097`'s loop over 0..=9 from `pg_prng_seed(12345)`, as
    /// the C prints it.
    #[test]
    fn the_shuffle_is_upstreams_inside_out_fisher_yates() {
        let mut items: Vec<u32> = (0..10).collect();
        Prng::seed(12345).shuffle(&mut items);
        assert_eq!(items, [0, 2, 9, 6, 4, 1, 8, 3, 7, 5]);
    }
}
