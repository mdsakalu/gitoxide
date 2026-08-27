//! A transaction for editing one physical configuration file.

use std::{
    io::Read,
    ops::{Deref, DerefMut},
};

use crate::bstr::{BStr, ByteSlice};

/// A locked, mutable physical configuration file.
///
/// Includes are not expanded. Dropping this value releases the lock and discards all changes;
/// [`commit()`](Self::commit()) writes them atomically. The owning repository is not updated.
pub struct FileMut {
    pub(crate) lock: gix_lock::File,
    pub(crate) config: gix_config::File,
}

/// The error produced when opening or committing a [`FileMut`].
#[derive(Debug, thiserror::Error)]
#[expect(missing_docs)]
pub enum Error {
    #[error(transparent)]
    LockTimeout(#[from] super::lock_timeout::Error),
    #[error("Invalid value for core.sharedRepository: {value:?}")]
    InvalidSharedRepository { value: crate::bstr::BString },
    #[error("Could not acquire the lock for the configuration file")]
    AcquireLock(#[from] gix_lock::acquire::Error),
    #[error("Could not read metadata of the configuration file at {path:?}")]
    Metadata {
        source: std::io::Error,
        path: std::path::PathBuf,
    },
    #[error("Could not read the configuration file at {path:?}")]
    Read {
        source: std::io::Error,
        path: std::path::PathBuf,
    },
    #[error("Could not parse the configuration file at {path:?}")]
    Parse {
        source: gix_config::file::init::Error,
        path: std::path::PathBuf,
    },
    #[error("Could not write the configuration file at {path:?}")]
    Write {
        source: std::io::Error,
        path: std::path::PathBuf,
    },
    #[error("Could not commit the configuration file lock")]
    CommitLock(#[from] gix_lock::commit::Error<gix_lock::File>),
}

impl FileMut {
    pub(crate) fn open(
        path: std::path::PathBuf,
        trust: gix_sec::Trust,
        lock_mode: gix_lock::acquire::Fail,
        shared_repository: i32,
    ) -> Result<Self, Error> {
        let mut lock = if shared_repository == 0 {
            gix_lock::File::acquire_to_update_resource_following_symlinks(&path, lock_mode, None)?
        } else {
            gix_lock::File::acquire_to_update_resource_following_symlinks_with_permissions(
                &path,
                lock_mode,
                None,
                |permissions| adjust_shared_repository_permissions(permissions, shared_repository),
            )?
        };
        let path = lock.resource_path();
        let config = match std::fs::File::open(&path) {
            Ok(mut file) => {
                let permissions = file
                    .metadata()
                    .map_err(|source| Error::Metadata {
                        source,
                        path: path.clone(),
                    })?
                    .permissions();
                lock.with_mut(|file| file.set_permissions(permissions))
                    .map_err(|source| Error::Write {
                        source,
                        path: path.clone(),
                    })?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).map_err(|source| Error::Read {
                    source,
                    path: path.clone(),
                })?;
                gix_config::File::from_bytes_no_includes(
                    &bytes,
                    gix_config::file::Metadata::from(gix_config::Source::Local)
                        .at(path.clone())
                        .with(trust),
                    Default::default(),
                )
                .map_err(|source| Error::Parse {
                    source,
                    path: path.clone(),
                })?
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => gix_config::File::new(
                gix_config::file::Metadata::from(gix_config::Source::Local)
                    .at(path)
                    .with(trust),
            ),
            Err(source) => return Err(Error::Read { source, path }),
        };
        Ok(FileMut { lock, config })
    }

    /// Write this physical file atomically without changing any repository instance.
    pub fn commit(mut self) -> Result<(), Error> {
        let path = self.lock.resource_path();
        self.config
            .write_to(&mut self.lock)
            .map_err(|source| Error::Write { source, path })?;
        self.lock.commit()?;
        Ok(())
    }
}

pub(crate) fn shared_repository(
    config: &gix_config::File,
    filter: fn(&gix_config::file::Metadata) -> bool,
) -> Result<i32, Error> {
    let value = config.sections_by_name_and_filter("core", filter).and_then(|sections| {
        sections
            .filter(|section| section.header().subsection_name().is_none())
            .filter_map(|section| section.value_implicit("sharedRepository"))
            .last()
    });
    match value {
        None => Ok(0),
        Some(None) => parse_shared_repository(None),
        Some(Some(value)) => parse_shared_repository(Some(value.as_bstr())),
    }
}

fn parse_shared_repository(value: Option<&BStr>) -> Result<i32, Error> {
    // Match Git's compact encoding: positive modes add bits, while negative modes replace them.
    let Some(value) = value else { return Ok(0o660) };
    match value.as_bytes() {
        b"umask" => return Ok(0),
        b"group" => return Ok(0o660),
        b"all" | b"world" | b"everybody" => return Ok(0o664),
        _ => {}
    }

    if let Some(mode) = value.to_str().ok().and_then(|value| u32::from_str_radix(value, 8).ok()) {
        return match mode {
            0 => Ok(0),
            1 => Ok(0o660),
            2 => Ok(0o664),
            mode if mode & 0o600 == 0o600 => Ok(-((mode & 0o666) as i32)),
            _ => Err(Error::InvalidSharedRepository { value: value.into() }),
        };
    }

    gix_config::Boolean::try_from(value)
        .map(|value| if value.0 { 0o660 } else { 0 })
        .map_err(|_| Error::InvalidSharedRepository { value: value.into() })
}

fn adjust_shared_repository_permissions(
    mut permissions: std::fs::Permissions,
    shared_repository: i32,
) -> std::fs::Permissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = permissions.mode();
        permissions.set_mode(if shared_repository < 0 {
            (mode & !0o777) | (-shared_repository) as u32
        } else {
            mode | shared_repository as u32
        });
    }
    #[cfg(not(unix))]
    let _ = shared_repository;
    permissions
}

impl Deref for FileMut {
    type Target = gix_config::File;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}

impl DerefMut for FileMut {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.config
    }
}

#[cfg(test)]
mod tests {
    use super::parse_shared_repository;
    use crate::bstr::BStr;

    #[test]
    fn shared_repository_values_match_git() {
        for (value, expected) in [
            (None, 0o660),
            (Some(b"umask".as_slice()), 0),
            (Some(b"false".as_slice()), 0),
            (Some(b"0".as_slice()), 0),
            (Some(b"group".as_slice()), 0o660),
            (Some(b"true".as_slice()), 0o660),
            (Some(b"1".as_slice()), 0o660),
            (Some(b"all".as_slice()), 0o664),
            (Some(b"world".as_slice()), 0o664),
            (Some(b"everybody".as_slice()), 0o664),
            (Some(b"2".as_slice()), 0o664),
            (Some(b"0640".as_slice()), -0o640),
        ] {
            assert_eq!(
                parse_shared_repository(value.map(BStr::new)).expect("valid shared-repository mode"),
                expected,
                "value {value:?}"
            );
        }
        assert!(
            parse_shared_repository(Some(BStr::new(b"0400"))).is_err(),
            "the owner must retain read and write access"
        );
    }
}
