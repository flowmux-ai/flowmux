// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows recovery writes without copying directory attributes onto files.
//!
//! The shared store still owns naming, JSON format, reads, removals and recovery
//! identity. Only its file-creation/replacement step differs on Windows. All
//! operations run on the editor I/O worker, never the UI thread.

use flowmux_editor::{RecoveryError, RecoveryOperation, RecoveryStore};

pub fn apply(store: &RecoveryStore, operation: &RecoveryOperation) -> Result<(), RecoveryError> {
    #[cfg(not(windows))]
    {
        store.apply(operation)
    }
    #[cfg(windows)]
    {
        match operation {
            RecoveryOperation::Write(snapshot) => native::write(store, snapshot),
            RecoveryOperation::Remove(path) => store.remove(path),
        }
    }
}

#[cfg(windows)]
mod native {
    use flowmux_editor::{
        RecoveryError, RecoverySnapshot, RecoveryStore, DEFAULT_MAX_DOCUMENT_BYTES,
        RECOVERY_FORMAT_VERSION,
    };
    use std::{
        ffi::c_void,
        fs::File,
        io::{self, Write},
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        path::{Path, PathBuf},
    };
    use windows_sys::Win32::{
        Foundation::{LocalFree, GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            GetTokenInformation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        },
        Storage::FileSystem::{
            CreateFileW, MoveFileExW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, MOVEFILE_REPLACE_EXISTING,
            MOVEFILE_WRITE_THROUGH,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    // Match the shared recovery reader's bound: JSON can expand content 6x.
    const MAX_RECOVERY_FILE_BYTES: u64 = 6 * DEFAULT_MAX_DOCUMENT_BYTES + 1024 * 1024;

    fn recovery_io(operation: &'static str, path: &Path, source: io::Error) -> RecoveryError {
        RecoveryError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }

    fn checked(result: i32) -> io::Result<()> {
        if result == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    struct LocalAllocation(*mut c_void);
    impl Drop for LocalAllocation {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }

    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut text: Vec<_> = path.as_os_str().encode_wide().collect();
        if text.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path contains NUL",
            ));
        }
        text.push(0);
        Ok(text)
    }

    fn current_user_sid() -> io::Result<String> {
        unsafe {
            let mut raw_token = std::ptr::null_mut();
            checked(OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY,
                &mut raw_token,
            ))?;
            let token = OwnedHandle::from_raw_handle(raw_token);
            let mut size = 0;
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &mut size,
            );
            if size == 0 {
                return Err(io::Error::last_os_error());
            }
            // TOKEN_USER includes pointers; retain pointer alignment for its buffer.
            let mut storage = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
            checked(GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                storage.as_mut_ptr().cast(),
                size,
                &mut size,
            ))?;
            let user = &*storage.as_ptr().cast::<TOKEN_USER>();
            sid_string(user.User.Sid)
        }
    }

    // The SID remains owned by the caller while ConvertSidToStringSidW runs.
    unsafe fn sid_string(sid: *mut c_void) -> io::Result<String> {
        let mut text = std::ptr::null_mut();
        checked(unsafe { ConvertSidToStringSidW(sid, &mut text) })?;
        let _allocation = LocalAllocation(text.cast());
        let length = (0..)
            .take_while(|index| unsafe { *text.add(*index) != 0 })
            .count();
        Ok(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(text, length)
        }))
    }

    fn private_descriptor() -> io::Result<LocalAllocation> {
        // D:P prevents parent ACL inheritance; only SYSTEM and this process's
        // user receive full access. CreateFile applies it before any byte write.
        // https://learn.microsoft.com/windows/win32/secauthz/security-descriptor-string-format
        let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{})", current_user_sid()?);
        let text: Vec<_> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut descriptor = std::ptr::null_mut();
        checked(unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        })?;
        Ok(LocalAllocation(descriptor))
    }

    struct Temporary {
        path: PathBuf,
        file: Option<File>,
        committed: bool,
    }
    impl Temporary {
        fn create(directory: &Path) -> io::Result<Self> {
            let path = directory.join(format!(".flowmux-recovery-{}.tmp", uuid::Uuid::new_v4()));
            let name = wide(&path)?;
            let descriptor = private_descriptor()?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: 0,
            };
            // CREATE_NEW never truncates an existing path, and share mode zero
            // prevents another open/rename until this writer closes its handle.
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_WRITE,
                    0,
                    &attributes,
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                // No Temporary exists on failure: never delete a name we did not create.
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                path,
                file: Some(unsafe { File::from_raw_handle(handle) }),
                committed: false,
            })
        }

        fn persist(mut self, destination: &Path) -> io::Result<()> {
            // File::sync_all was completed before releasing this private handle.
            drop(self.file.take());
            checked(unsafe {
                MoveFileExW(
                    wide(&self.path)?.as_ptr(),
                    wide(destination)?.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            })?;
            // Both names use the same canonical parent and COPY_ALLOWED is not
            // set: no cross-volume copy or delete-destination fallback is used.
            self.committed = true;
            Ok(())
        }
    }
    impl Drop for Temporary {
        fn drop(&mut self) {
            drop(self.file.take());
            if !self.committed {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }

    pub(super) fn write(
        store: &RecoveryStore,
        snapshot: &RecoverySnapshot,
    ) -> Result<(), RecoveryError> {
        let path = store.snapshot_path(&snapshot.identity_path);
        if snapshot.format_version != RECOVERY_FORMAT_VERSION {
            return Err(RecoveryError::UnsupportedFormat {
                path,
                actual: snapshot.format_version,
            });
        }
        if snapshot.workspace_id != store.workspace_id() {
            return Err(RecoveryError::WrongWorkspace { path });
        }
        if snapshot.content.len() > DEFAULT_MAX_DOCUMENT_BYTES as usize {
            return Err(RecoveryError::TooLarge {
                path,
                limit: DEFAULT_MAX_DOCUMENT_BYTES,
            });
        }
        let bytes = serde_json::to_vec(snapshot).map_err(|source| RecoveryError::InvalidJson {
            path: path.clone(),
            source,
        })?;
        if bytes.len() as u64 > MAX_RECOVERY_FILE_BYTES {
            return Err(RecoveryError::TooLarge {
                path,
                limit: MAX_RECOVERY_FILE_BYTES,
            });
        }

        // Canonical Windows paths carry the extended prefix, so native calls do
        // not reintroduce the MAX_PATH limit for the recovery directory.
        let parent = path.parent().ok_or_else(|| {
            recovery_io(
                "locate snapshot directory",
                &path,
                io::Error::new(io::ErrorKind::InvalidInput, "snapshot has no parent"),
            )
        })?;
        let directory = std::fs::canonicalize(parent)
            .map_err(|source| recovery_io("resolve snapshot directory", &path, source))?;
        let destination = directory.join(path.file_name().ok_or_else(|| {
            recovery_io(
                "locate snapshot name",
                &path,
                io::Error::new(io::ErrorKind::InvalidInput, "snapshot has no name"),
            )
        })?);
        let mut temporary = Temporary::create(&directory)
            .map_err(|source| recovery_io("create private temporary file", &path, source))?;
        let file = temporary.file.as_mut().unwrap();
        file.write_all(&bytes)
            .map_err(|source| recovery_io("write snapshot", &path, source))?;
        file.sync_all()
            .map_err(|source| recovery_io("flush snapshot", &path, source))?;
        temporary
            .persist(&destination)
            .map_err(|source| recovery_io("replace snapshot", &path, source))
    }

    #[cfg(test)]
    pub(super) fn assert_private_dacl(path: &Path) {
        use windows_sys::Win32::{
            Foundation::GENERIC_ALL,
            Security::{
                Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT},
                GetAce, GetSecurityDescriptorControl, ACCESS_ALLOWED_ACE,
                DACL_SECURITY_INFORMATION, INHERITED_ACE, SE_DACL_PROTECTED,
            },
            Storage::FileSystem::FILE_ALL_ACCESS,
            System::SystemServices::ACCESS_ALLOWED_ACE_TYPE,
        };
        unsafe {
            let mut descriptor = std::ptr::null_mut();
            let mut acl = std::ptr::null_mut();
            let status = GetNamedSecurityInfoW(
                wide(&std::fs::canonicalize(path).unwrap())
                    .unwrap()
                    .as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut acl,
                std::ptr::null_mut(),
                &mut descriptor,
            );
            assert_eq!(status, 0, "GetNamedSecurityInfoW: {status}");
            let _allocation = LocalAllocation(descriptor);
            let mut control = 0;
            let mut revision = 0;
            checked(GetSecurityDescriptorControl(
                descriptor,
                &mut control,
                &mut revision,
            ))
            .unwrap();
            assert_ne!(control & SE_DACL_PROTECTED, 0);
            assert!(!acl.is_null());
            assert_eq!((*acl).AceCount, 2);
            let mut actual = Vec::new();
            for index in 0..2 {
                let mut raw_ace = std::ptr::null_mut();
                checked(GetAce(acl, index, &mut raw_ace)).unwrap();
                let ace = &*raw_ace.cast::<ACCESS_ALLOWED_ACE>();
                assert_eq!(ace.Header.AceType as u32, ACCESS_ALLOWED_ACE_TYPE);
                assert_eq!(ace.Header.AceFlags as u32 & INHERITED_ACE, 0);
                assert!(
                    ace.Mask & FILE_ALL_ACCESS == FILE_ALL_ACCESS || ace.Mask & GENERIC_ALL != 0
                );
                actual.push(sid_string((&ace.SidStart as *const u32).cast_mut().cast()).unwrap());
            }
            let mut expected = vec!["S-1-5-18".to_owned(), current_user_sid().unwrap()];
            actual.sort();
            expected.sort();
            assert_eq!(actual, expected);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_editor::{LineEnding, RecoveryDiskState, RecoverySnapshot, TextEncoding};
    use std::{fs, path::PathBuf};

    struct Fixture {
        root: PathBuf,
        store: RecoveryStore,
        snapshot: RecoverySnapshot,
    }
    impl Fixture {
        fn new() -> Self {
            Self::with_nested_root(false)
        }
        fn with_nested_root(long: bool) -> Self {
            let root = std::env::temp_dir()
                .join(format!("flowmux-recovery-한글-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&root).unwrap();
            let mut workspace = root.clone();
            if long {
                for _ in 0..4 {
                    workspace.push("nested".repeat(12));
                }
                fs::create_dir_all(&workspace).unwrap();
            }
            let file = workspace.join("한 é 😀.txt");
            let base = "\u{feff}original\r\n".as_bytes();
            fs::write(&file, base).unwrap();
            let store =
                RecoveryStore::new_scoped(workspace.join("state"), &workspace, "surface-one")
                    .unwrap();
            let snapshot = RecoverySnapshot::new(
                store.workspace_id().into(),
                fs::canonicalize(file).unwrap(),
                base,
                7,
                "한글 한 é 😀\nunsaved\n".into(),
                TextEncoding::Utf8Bom,
                LineEnding::CrLf,
            );
            Self {
                root,
                store,
                snapshot,
            }
        }
        fn write(&self, snapshot: &RecoverySnapshot) -> Result<(), RecoveryError> {
            apply(&self.store, &RecoveryOperation::Write(snapshot.clone()))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn recovery_round_trip_replace_remove_preserves_unicode_and_shared_format() {
        let fixture = Fixture::new();
        fixture.write(&fixture.snapshot).unwrap();
        let identity = &fixture.snapshot.identity_path;
        let path = fixture.store.snapshot_path(identity);
        assert_eq!(
            fixture.store.read(identity).unwrap(),
            Some((fixture.snapshot.clone(), RecoveryDiskState::Unchanged))
        );
        #[cfg(windows)]
        native::assert_private_dacl(&path);
        let mut replacement = fixture.snapshot.clone();
        replacement.document_version += 1;
        replacement.content.push_str("replacement 한 é 😀\n");
        fixture.write(&replacement).unwrap();
        assert_eq!(
            fixture.store.read(identity).unwrap().unwrap().0,
            replacement
        );
        #[cfg(windows)]
        native::assert_private_dacl(&path);
        assert_eq!(
            fs::read(identity).unwrap(),
            "\u{feff}original\r\n".as_bytes()
        );
        apply(&fixture.store, &RecoveryOperation::Remove(identity.clone())).unwrap();
        assert!(!path.exists());
        assert!(fixture.store.read(identity).unwrap().is_none());
        apply(&fixture.store, &RecoveryOperation::Remove(identity.clone())).unwrap();
    }

    #[test]
    fn recovery_validation_failure_preserves_previous_snapshot() {
        let fixture = Fixture::new();
        fixture.write(&fixture.snapshot).unwrap();
        let path = fixture.store.snapshot_path(&fixture.snapshot.identity_path);
        let original = fs::read(&path).unwrap();
        let mut invalid = fixture.snapshot.clone();
        invalid.workspace_id = "another-workspace".into();
        assert!(matches!(
            fixture.write(&invalid),
            Err(RecoveryError::WrongWorkspace { .. })
        ));
        invalid = fixture.snapshot.clone();
        invalid.content = "x".repeat(flowmux_editor::DEFAULT_MAX_DOCUMENT_BYTES as usize + 1);
        assert!(matches!(
            fixture.write(&invalid),
            Err(RecoveryError::TooLarge { .. })
        ));
        #[cfg(windows)]
        {
            invalid = fixture.snapshot.clone();
            invalid.format_version += 1;
            assert!(matches!(
                fixture.write(&invalid),
                Err(RecoveryError::UnsupportedFormat { .. })
            ));
        }
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn native_recovery_locked_replace_preserves_old_bytes_and_cleans_temporary_file() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let fixture = Fixture::new();
        fixture.write(&fixture.snapshot).unwrap();
        let path = fixture.store.snapshot_path(&fixture.snapshot.identity_path);
        let original = fs::read(&path).unwrap();
        // Deny delete/rename sharing while allowing inspection of the old bytes.
        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)
            .unwrap();
        let mut changed = fixture.snapshot.clone();
        changed.content = "must not replace while locked".into();
        assert!(matches!(
            fixture.write(&changed),
            Err(RecoveryError::Io {
                operation: "replace snapshot",
                ..
            })
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        drop(lock);
        fixture.write(&changed).unwrap();
        assert_eq!(
            fixture
                .store
                .read(&changed.identity_path)
                .unwrap()
                .unwrap()
                .0,
            changed
        );
        native::assert_private_dacl(&path);
    }

    #[cfg(windows)]
    #[test]
    fn native_recovery_extended_paths_preserve_shared_read_remove_identity() {
        use std::os::windows::ffi::OsStrExt;
        let fixture = Fixture::with_nested_root(true);
        let identity = &fixture.snapshot.identity_path;
        let path = fixture.store.snapshot_path(identity);
        assert!(path.as_os_str().encode_wide().count() > 400);
        fixture.write(&fixture.snapshot).unwrap();
        native::assert_private_dacl(&path);
        assert_eq!(
            fixture.store.read(identity).unwrap().unwrap().0,
            fixture.snapshot
        );
        let mut changed = fixture.snapshot.clone();
        changed.content.push_str("long-path replacement 한 é\n");
        fixture.write(&changed).unwrap();
        assert_eq!(fixture.store.read(identity).unwrap().unwrap().0, changed);
        apply(&fixture.store, &RecoveryOperation::Remove(identity.clone())).unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 0);
    }
}
