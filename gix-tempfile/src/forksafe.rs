use std::path::Path;

use tempfile::{NamedTempFile, TempPath};

use crate::{AutoRemove, handle};

#[cfg(windows)]
fn persist_windows(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::{iter, os::windows::ffi::OsStrExt};

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_TEMPORARY, GetFileAttributesW,
        INVALID_FILE_ATTRIBUTES, MOVEFILE_REPLACE_EXISTING, MoveFileExW, SetFileAttributesW,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(iter::once(0)).collect()
    }

    fn usable_attributes(attributes: u32) -> u32 {
        if attributes == 0 {
            FILE_ATTRIBUTE_NORMAL
        } else {
            attributes
        }
    }

    let source = wide(source);
    let destination = wide(destination);
    // SAFETY: Both paths are encoded as NUL-terminated UTF-16 strings and remain alive for all calls.
    #[expect(unsafe_code)]
    unsafe {
        let source_attributes = GetFileAttributesW(source.as_ptr());
        if source_attributes == INVALID_FILE_ATTRIBUTES {
            return Err(std::io::Error::last_os_error());
        }
        let persisted_attributes = usable_attributes(source_attributes & !FILE_ATTRIBUTE_TEMPORARY);
        if SetFileAttributesW(source.as_ptr(), persisted_attributes) == 0 {
            return Err(std::io::Error::last_os_error());
        }

        let destination_attributes = GetFileAttributesW(destination.as_ptr());
        let destination_was_readonly =
            destination_attributes != INVALID_FILE_ATTRIBUTES && destination_attributes & FILE_ATTRIBUTE_READONLY != 0;
        if destination_was_readonly
            && SetFileAttributesW(
                destination.as_ptr(),
                usable_attributes(destination_attributes & !FILE_ATTRIBUTE_READONLY),
            ) == 0
        {
            let err = std::io::Error::last_os_error();
            let _ = SetFileAttributesW(source.as_ptr(), source_attributes);
            return Err(err);
        }

        if MoveFileExW(source.as_ptr(), destination.as_ptr(), MOVEFILE_REPLACE_EXISTING) != 0 {
            return Ok(());
        }

        let err = std::io::Error::last_os_error();
        let _ = SetFileAttributesW(source.as_ptr(), source_attributes);
        if destination_was_readonly {
            let _ = SetFileAttributesW(destination.as_ptr(), destination_attributes);
        }
        Err(err)
    }
}

enum TempfileOrTemppath {
    Tempfile(NamedTempFile),
    Temppath(TempPath),
}

pub(crate) struct ForksafeTempfile {
    inner: TempfileOrTemppath,
    cleanup: AutoRemove,
    pub owning_process_id: u32,
}

impl ForksafeTempfile {
    pub fn new(tempfile: NamedTempFile, cleanup: AutoRemove, mode: handle::Mode) -> Self {
        use handle::Mode::*;
        ForksafeTempfile {
            inner: match mode {
                Closed => TempfileOrTemppath::Temppath(tempfile.into_temp_path()),
                Writable => TempfileOrTemppath::Tempfile(tempfile),
            },
            cleanup,
            owning_process_id: std::process::id(),
        }
    }
}

impl ForksafeTempfile {
    pub fn as_mut_tempfile(&mut self) -> Option<&mut NamedTempFile> {
        match &mut self.inner {
            TempfileOrTemppath::Tempfile(file) => Some(file),
            TempfileOrTemppath::Temppath(_) => None,
        }
    }
    pub fn close(self) -> Self {
        if let TempfileOrTemppath::Tempfile(file) = self.inner {
            ForksafeTempfile {
                inner: TempfileOrTemppath::Temppath(file.into_temp_path()),
                cleanup: self.cleanup,
                owning_process_id: self.owning_process_id,
            }
        } else {
            self
        }
    }
    pub fn persist(self, path: impl AsRef<Path>) -> Result<Option<std::fs::File>, (std::io::Error, Self)> {
        self.persist_inner(path.as_ref())
    }

    #[cfg(windows)]
    fn persist_inner(mut self, path: &Path) -> Result<Option<std::fs::File>, (std::io::Error, Self)> {
        /// Maximum number of attempts for Windows file locking issues.
        /// Matches libgit2's default retry count.
        const MAX_ATTEMPTS: usize = 10;
        /// Delay between retry attempts in milliseconds.
        /// Matches libgit2's retry delay.
        const RETRY_DELAY_MS: u64 = 5;

        fn should_retry(err: &std::io::Error) -> bool {
            use std::io::ErrorKind;
            // Access denied (ERROR_ACCESS_DENIED = 5) or sharing violation (ERROR_SHARING_VIOLATION = 32)
            // are the common errors when external processes like antivirus or file watchers hold the file.
            matches!(err.kind(), ErrorKind::PermissionDenied) || err.raw_os_error() == Some(32)
            // ERROR_SHARING_VIOLATION
        }

        match self.inner {
            TempfileOrTemppath::Tempfile(file) => {
                let mut current_file = file;
                for attempt in 0..MAX_ATTEMPTS {
                    match persist_windows(current_file.path(), path) {
                        Ok(()) => {
                            current_file.disable_cleanup(true);
                            return Ok(Some(current_file.into_file()));
                        }
                        Err(err) if attempt + 1 < MAX_ATTEMPTS && should_retry(&err) => {
                            std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
                        }
                        Err(err) => {
                            return Err((err, {
                                self.inner = TempfileOrTemppath::Tempfile(current_file);
                                self
                            }));
                        }
                    }
                }
                unreachable!("loop always returns")
            }
            TempfileOrTemppath::Temppath(temppath) => {
                let mut current_path = temppath;
                for attempt in 0..MAX_ATTEMPTS {
                    match persist_windows(&current_path, path) {
                        Ok(()) => {
                            current_path.disable_cleanup(true);
                            return Ok(None);
                        }
                        Err(err) if attempt + 1 < MAX_ATTEMPTS && should_retry(&err) => {
                            std::thread::sleep(std::time::Duration::from_millis(RETRY_DELAY_MS));
                        }
                        Err(err) => {
                            return Err((err, {
                                self.inner = TempfileOrTemppath::Temppath(current_path);
                                self
                            }));
                        }
                    }
                }
                unreachable!("loop always returns")
            }
        }
    }

    #[cfg(not(windows))]
    fn persist_inner(mut self, path: &Path) -> Result<Option<std::fs::File>, (std::io::Error, Self)> {
        match self.inner {
            TempfileOrTemppath::Tempfile(file) => match file.persist(path) {
                Ok(file) => Ok(Some(file)),
                Err(err) => Err((err.error, {
                    self.inner = TempfileOrTemppath::Tempfile(err.file);
                    self
                })),
            },
            TempfileOrTemppath::Temppath(temppath) => match temppath.persist(path) {
                Ok(_) => Ok(None),
                Err(err) => Err((err.error, {
                    self.inner = TempfileOrTemppath::Temppath(err.path);
                    self
                })),
            },
        }
    }

    pub fn into_temppath(self) -> TempPath {
        match self.inner {
            TempfileOrTemppath::Tempfile(file) => file.into_temp_path(),
            TempfileOrTemppath::Temppath(path) => path,
        }
    }
    pub fn into_tempfile(self) -> Option<NamedTempFile> {
        match self.inner {
            TempfileOrTemppath::Tempfile(file) => Some(file),
            TempfileOrTemppath::Temppath(_) => None,
        }
    }
    pub fn drop_impl(self) {
        let file_path = match self.inner {
            TempfileOrTemppath::Tempfile(file) => file.path().to_owned(),
            TempfileOrTemppath::Temppath(path) => path.to_path_buf(),
        };
        let parent_directory = file_path.parent().expect("every tempfile has a parent directory");
        self.cleanup.execute_best_effort(parent_directory);
    }

    pub fn drop_without_deallocation(self) {
        use std::io::Write;
        let temppath = match self.inner {
            TempfileOrTemppath::Tempfile(file) => {
                let (mut file, temppath) = file.into_parts();
                file.flush().ok();
                temppath
            }
            TempfileOrTemppath::Temppath(path) => path,
        };
        std::fs::remove_file(&temppath).ok();
        std::mem::forget(
            self.cleanup
                .execute_best_effort(temppath.parent().expect("every file has a directory")),
        );
        std::mem::forget(temppath); // leak memory to prevent deallocation
    }
}
