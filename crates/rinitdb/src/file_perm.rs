//! The data-directory permission constants, ported from
//! `src/include/common/file_perm.h` and `src/common/file_perm.c`.
//!
//! C keeps `pg_dir_create_mode`, `pg_file_create_mode` and `pg_mode_mask` as
//! three process globals that `SetDataDirectoryCreatePerm` writes from the
//! `-g` switch arm (`initdb.c:3360`). Globals are not data, so the same three
//! numbers travel as one [`DataDirPerm`] value inside the plan instead.

/// `PG_MODE_MASK_OWNER` (`file_perm.h:24`): `S_IRWXG | S_IRWXO`.
pub const PG_MODE_MASK_OWNER: u32 = 0o077;
/// `PG_MODE_MASK_GROUP` (`file_perm.h:29`): `S_IWGRP | S_IRWXO`.
pub const PG_MODE_MASK_GROUP: u32 = 0o027;
/// `PG_DIR_MODE_OWNER` (`file_perm.h:32`): `S_IRWXU`.
pub const PG_DIR_MODE_OWNER: u32 = 0o700;
/// `PG_DIR_MODE_GROUP` (`file_perm.h:35`): `S_IRWXU | S_IRGRP | S_IXGRP`.
pub const PG_DIR_MODE_GROUP: u32 = 0o750;
/// `PG_FILE_MODE_OWNER` (`file_perm.h:38`): `S_IRUSR | S_IWUSR`.
pub const PG_FILE_MODE_OWNER: u32 = 0o600;
/// `PG_FILE_MODE_GROUP` (`file_perm.h:41`): `S_IRUSR | S_IWUSR | S_IRGRP`.
pub const PG_FILE_MODE_GROUP: u32 = 0o640;

/// The three modes `initdb` creates PGDATA with.
///
/// `mode_mask` is what `initialize_data_directory` passes to `umask()`
/// (`initdb.c:3058`); it is carried because it is one of the three values
/// `SetDataDirectoryCreatePerm` sets, and because the arithmetic it implies is
/// what lets [`crate::layout`] state a final mode per entry — see
/// [`DataDirPerm::masked_dir_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataDirPerm {
    /// `pg_dir_create_mode`.
    pub dir_mode: u32,
    /// `pg_file_create_mode`.
    pub file_mode: u32,
    /// `pg_mode_mask`.
    pub mode_mask: u32,
}

impl DataDirPerm {
    /// The `file_perm.c:18` defaults, owner-only.
    pub const OWNER: Self = Self {
        dir_mode: PG_DIR_MODE_OWNER,
        file_mode: PG_FILE_MODE_OWNER,
        mode_mask: PG_MODE_MASK_OWNER,
    };

    /// What `--allow-group-access` relaxes them to.
    pub const GROUP: Self = Self {
        dir_mode: PG_DIR_MODE_GROUP,
        file_mode: PG_FILE_MODE_GROUP,
        mode_mask: PG_MODE_MASK_GROUP,
    };

    /// `SetDataDirectoryCreatePerm(dataDirMode)` (`file_perm.c:34`): group
    /// read *and* execute in the mode relaxes all three, anything else does
    /// not.
    #[must_use]
    pub fn set_data_directory_create_perm(data_dir_mode: u32) -> Self {
        if PG_DIR_MODE_GROUP & data_dir_mode == PG_DIR_MODE_GROUP {
            Self::GROUP
        } else {
            Self::OWNER
        }
    }

    /// The `-g` / `--allow-group-access` switch arm (`initdb.c:3359`), which
    /// is the only caller in initdb.
    #[must_use]
    pub fn for_allow_group_access(allow_group_access: bool) -> Self {
        if allow_group_access {
            Self::set_data_directory_create_perm(PG_DIR_MODE_GROUP)
        } else {
            Self::OWNER
        }
    }

    /// The mode a directory actually ends up with: `mkdir(path, dir_mode)`
    /// under `umask(mode_mask)`.
    ///
    /// For both of the two settings this is `dir_mode` unchanged — the mask
    /// only ever clears bits `dir_mode` does not carry — which is what lets
    /// the layout name one final mode per entry rather than modelling a
    /// process-wide umask. `the_mask_never_touches_the_create_modes` pins it.
    #[must_use]
    pub fn masked_dir_mode(self) -> u32 {
        self.dir_mode & !self.mode_mask
    }

    /// The mode a file actually ends up with: `fopen` asks for 0666 and
    /// `umask(mode_mask)` takes it down to `file_create_mode`
    /// (`write_version_file`, `initdb.c:1024`, creates PG_VERSION this way).
    #[must_use]
    pub fn masked_file_mode(self) -> u32 {
        0o666 & !self.mode_mask
    }
}

impl Default for DataDirPerm {
    fn default() -> Self {
        Self::OWNER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_owner_only() {
        assert_eq!(
            DataDirPerm::for_allow_group_access(false),
            DataDirPerm::OWNER
        );
        assert_eq!(DataDirPerm::default(), DataDirPerm::OWNER);
    }

    #[test]
    fn allow_group_access_relaxes_all_three() {
        let perm = DataDirPerm::for_allow_group_access(true);
        assert_eq!(perm.dir_mode, 0o750);
        assert_eq!(perm.file_mode, 0o640);
        assert_eq!(perm.mode_mask, 0o027);
    }

    #[test]
    fn group_read_without_group_execute_is_not_group_access() {
        // file_perm.c:37 tests both bits, so 0740 is not enough.
        assert_eq!(
            DataDirPerm::set_data_directory_create_perm(0o740),
            DataDirPerm::OWNER
        );
        assert_eq!(
            DataDirPerm::set_data_directory_create_perm(0o750),
            DataDirPerm::GROUP
        );
        // Extra bits beyond PG_DIR_MODE_GROUP still count as group access,
        // which is how GetDataDirectoryCreatePerm's callers behave.
        assert_eq!(
            DataDirPerm::set_data_directory_create_perm(0o770),
            DataDirPerm::GROUP
        );
    }

    #[test]
    fn the_mask_never_touches_the_create_modes() {
        for perm in [DataDirPerm::OWNER, DataDirPerm::GROUP] {
            assert_eq!(perm.masked_dir_mode(), perm.dir_mode);
            assert_eq!(perm.masked_file_mode(), perm.file_mode);
        }
    }
}
