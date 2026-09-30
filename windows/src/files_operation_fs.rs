// SPDX-License-Identifier: GPL-3.0-or-later
//! F08 filesystem adapter; native acceptance is tracked separately.
//! API: Request { kind, root: PathBuf, source: String, destination: String };
//! Control { cancelled: Arc<AtomicBool>, deadline: Instant };
//! Prepared::prepare(Request, &Control) -> Result<Prepared> is read-only;
//! Prepared::commit(self, &Control) -> Result<Outcome> performs mutation.
//! Host owns admission, editor reservations and acceptance before commit.
//! Once publication succeeds, return Outcome even if cancellation became true.
//! All directories are pinned without DELETE sharing, from drive root downward.
//! Child opens/creates are handle-relative, so a reparse metadata change cannot
//! redirect a fresh pathname traversal. The isolated NtCreateFile ABI matches
//! installed windows-sys 0.61.2 Wdk definitions; native acceptance is required.

use anyhow::{ensure, Context};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

pub const MAX_COPY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_PATH_UNITS: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Copy,
    Rename,
    Move,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub kind: Kind,
    pub root: PathBuf,
    pub source: String,
    pub destination: String,
}

#[derive(Clone)]
pub struct Control {
    pub cancelled: Arc<AtomicBool>,
    pub deadline: Instant,
}
impl Control {
    pub fn check(&self) -> anyhow::Result<()> {
        ensure!(
            !self.cancelled.load(Ordering::Acquire),
            "file operation cancelled before commit"
        );
        ensure!(
            Instant::now() < self.deadline,
            "file operation deadline expired before commit"
        );
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Identity {
    pub volume: u32,
    pub index: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub kind: Kind,
    pub source: String,
    pub destination: String,
    pub source_identity: Identity,
    pub destination_identity: Identity,
    pub bytes: u64,
    pub warnings: Vec<String>,
}

#[cfg_attr(not(windows), allow(dead_code))]
struct Validated {
    request: Request,
    drive: String,
    root_parts: Vec<String>,
    source: Vec<String>,
    destination: Vec<String>,
}

fn component(value: &str) -> anyhow::Result<()> {
    ensure!(
        !value.is_empty() && value.encode_utf16().count() <= 255,
        "invalid file name length"
    );
    ensure!(
        !value.ends_with([' ', '.']),
        "trailing space/dot is unsupported"
    );
    ensure!(
        !value.chars().any(|c| c < ' ' || "<>:\"/\\|?*".contains(c)),
        "invalid file name character"
    );
    let stem = value.split('.').next().unwrap().to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL", "CLOCK$", "CONIN$", "CONOUT$"]
        .contains(&stem.as_str())
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        });
    ensure!(!reserved, "Windows device names are unsupported");
    Ok(())
}
#[cfg(any(windows, test))]
fn ordinary_case_flags(flags: u32) -> anyhow::Result<()> {
    // Value 1 means FILE_CS_FLAG_CASE_SENSITIVE_DIR. Unknown flags also fail closed.
    ensure!(
        flags == 0,
        "case-sensitive directory flags are unsupported for file operations"
    );
    Ok(())
}
fn relative(value: &str) -> anyhow::Result<Vec<String>> {
    ensure!(
        value.encode_utf16().count() <= MAX_PATH_UNITS,
        "relative path too long"
    );
    let normalized = value.replace('\\', "/");
    let parts: Vec<_> = normalized.split('/').map(str::to_owned).collect();
    ensure!(parts.len() <= MAX_DEPTH, "relative path too deep");
    for part in &parts {
        component(part)?;
    }
    Ok(parts)
}
fn validate(mut request: Request) -> anyhow::Result<Validated> {
    let root = request
        .root
        .to_str()
        .context("root is not Unicode")?
        .replace('/', "\\");
    let bytes = root.as_bytes();
    ensure!(
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\',
        "root must be an ordinary absolute local drive path"
    );
    ensure!(
        root.encode_utf16().count() <= MAX_PATH_UNITS,
        "root too long"
    );
    let drive = root[..3].to_owned();
    let tail = root[3..].trim_end_matches('\\');
    let root_parts = if tail.is_empty() {
        Vec::new()
    } else {
        relative(tail)?
    };
    let source = relative(&request.source)?;
    let mut destination = relative(&request.destination)?;
    if request.kind == Kind::Rename && destination.len() == 1 {
        let leaf = destination.pop().unwrap();
        destination = source[..source.len() - 1].to_vec();
        destination.push(leaf);
    }
    ensure!(
        root_parts.len() + source.len() <= MAX_DEPTH
            && root_parts.len() + destination.len() <= MAX_DEPTH,
        "combined path too deep"
    );
    ensure!(
        root.encode_utf16().count() + request.source.encode_utf16().count() < MAX_PATH_UNITS
            && root.encode_utf16().count() + request.destination.encode_utf16().count()
                < MAX_PATH_UNITS,
        "combined path too long"
    );
    request.source = source.join("/");
    request.destination = destination.join("/");
    ensure!(
        !request.source.eq_ignore_ascii_case(&request.destination),
        "source and destination must differ"
    );
    if request.kind == Kind::Rename {
        ensure!(
            source[..source.len() - 1] == destination[..destination.len() - 1],
            "rename must remain in the same directory"
        );
    }
    request.root = PathBuf::from(root);
    Ok(Validated {
        request,
        drive,
        root_parts,
        source,
        destination,
    })
}

pub struct Prepared {
    #[cfg(windows)]
    inner: native::Prepared,
}
impl Prepared {
    pub fn prepare(request: Request, control: &Control) -> anyhow::Result<Self> {
        control.check()?;
        let validated = validate(request)?;
        #[cfg(windows)]
        {
            Ok(Self {
                inner: native::Prepared::prepare(validated, control)?,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = validated;
            anyhow::bail!("native file operations require Windows")
        }
    }
    pub fn commit(self, control: &Control) -> anyhow::Result<Outcome> {
        control.check()?;
        #[cfg(windows)]
        {
            self.inner.commit(control)
        }
        #[cfg(not(windows))]
        {
            anyhow::bail!("native file operations require Windows")
        }
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{
        ffi::c_void,
        fs::{File, OpenOptions},
        io::{self, Read, Write},
        mem::size_of,
        os::windows::{
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle},
        },
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Foundation::{RtlNtStatusToDosError, HANDLE, UNICODE_STRING},
        Storage::FileSystem::*,
        System::IO::IO_STATUS_BLOCK,
    };

    // Isolated ABI from windows-sys 0.61.2 Wdk/Foundation + Wdk/Storage/FileSystem.
    // Avoid expanding production feature selection until the draft is integrated.
    #[repr(C)]
    struct ObjectAttributes {
        length: u32,
        root: HANDLE,
        name: *const UNICODE_STRING,
        attributes: u32,
        security_descriptor: *const c_void,
        security_quality: *const c_void,
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtCreateFile(
            file: *mut HANDLE,
            access: u32,
            attributes: *const ObjectAttributes,
            status: *mut IO_STATUS_BLOCK,
            allocation: *const i64,
            file_attributes: u32,
            sharing: u32,
            disposition: u32,
            options: u32,
            ea: *const c_void,
            ea_bytes: u32,
        ) -> i32;
        fn NtSetInformationFile(
            file: HANDLE,
            status: *mut IO_STATUS_BLOCK,
            information: *const c_void,
            length: u32,
            information_class: i32,
        ) -> i32;
    }
    const FILE_OPEN: u32 = 1;
    const FILE_CREATE: u32 = 2;
    const FILE_DIRECTORY_FILE: u32 = 1;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
    const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_RENAME_INFORMATION_CLASS: i32 = 10;

    fn child(
        parent: &File,
        name: &str,
        access: u32,
        sharing: u32,
        directory: bool,
        create: bool,
    ) -> anyhow::Result<File> {
        component(name)?;
        let mut wide: Vec<u16> = name.encode_utf16().collect();
        let count = u16::try_from(wide.len() * 2)?;
        let string = UNICODE_STRING {
            Length: count,
            MaximumLength: count,
            Buffer: wide.as_mut_ptr(),
        };
        let attributes = ObjectAttributes {
            length: size_of::<ObjectAttributes>() as u32,
            root: parent.as_raw_handle(),
            name: &string,
            attributes: 0x40,
            security_descriptor: null(),
            security_quality: null(),
        };
        let mut status = IO_STATUS_BLOCK::default();
        let mut handle = null_mut();
        // All pointers reference initialized stack/heap storage retained for this
        // synchronous call. The UTF-16 byte count fits UNICODE_STRING and names
        // contain one validated component. RootDirectory is an owned live handle;
        // OPEN_REPARSE_POINT opens a changed reparse leaf instead of traversing it.
        // Successful handle ownership transfers exactly once into File below.
        let result = unsafe {
            NtCreateFile(
                &mut handle,
                access | SYNCHRONIZE,
                &attributes,
                &mut status,
                null(),
                FILE_ATTRIBUTE_NORMAL,
                sharing,
                if create { FILE_CREATE } else { FILE_OPEN },
                FILE_SYNCHRONOUS_IO_NONALERT
                    | FILE_OPEN_REPARSE_POINT
                    | if directory {
                        FILE_DIRECTORY_FILE
                    } else {
                        FILE_NON_DIRECTORY_FILE
                    },
                null(),
                0,
            )
        };
        if result < 0 {
            return Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(result) } as i32,
            ))
            .context("handle-relative file open/create failed");
        }
        ensure!(!handle.is_null(), "native open returned no handle");
        Ok(unsafe { File::from_raw_handle(handle) })
    }
    fn information(file: &File, directory: bool) -> anyhow::Result<BY_HANDLE_FILE_INFORMATION> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error()).context("cannot read native identity");
        }
        ensure!(
            info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "reparse entries are unsupported"
        );
        ensure!(
            (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) == directory,
            "unexpected entry kind"
        );
        if directory {
            // FileCaseSensitiveInformation is available starting with Windows 10
            // version 1803. Require the query to succeed: older systems or file
            // systems that cannot report these flags cannot safely admit this
            // first-stage adapter. FILE_READ_ATTRIBUTES is held on every parent.
            // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntqueryinformationfile
            let mut case = FILE_CASE_SENSITIVE_INFO::default();
            if unsafe {
                GetFileInformationByHandleEx(
                    file.as_raw_handle(),
                    FileCaseSensitiveInfo,
                    (&mut case as *mut FILE_CASE_SENSITIVE_INFO).cast(),
                    size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error())
                    .context("cannot verify directory case sensitivity; file operation refused");
            }
            ordinary_case_flags(case.Flags)?;
        }
        Ok(info)
    }
    fn ensure_copy_has_no_named_streams(file: &File) -> anyhow::Result<()> {
        // ponytail: byte copies cannot preserve ADS (including Mark-of-the-Web).
        // Refuse them until a stream-preserving copy is implemented. Oversized
        // stream inventories fail closed too; query the pinned source handle.
        let mut storage = vec![0u64; 8192];
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileStreamInfo,
                storage.as_mut_ptr().cast(),
                (storage.len() * size_of::<u64>()) as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error())
                .context("cannot verify file streams; copy refused");
        }
        let info = unsafe { &*storage.as_ptr().cast::<FILE_STREAM_INFO>() };
        let unnamed: Vec<u16> = "::$DATA".encode_utf16().collect();
        ensure!(
            info.NextEntryOffset == 0
                && info.StreamNameLength as usize == unnamed.len() * 2
                && unsafe { std::slice::from_raw_parts(info.StreamName.as_ptr(), unnamed.len()) }
                    == unnamed,
            "Copying files with alternate data streams is unsupported; source left unchanged"
        );
        Ok(())
    }
    fn identity(info: &BY_HANDLE_FILE_INFORMATION) -> Identity {
        Identity {
            volume: info.dwVolumeSerialNumber,
            index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        }
    }
    fn length(info: &BY_HANDLE_FILE_INFORMATION) -> u64 {
        ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64
    }
    fn same_name(left: &str, right: &str) -> bool {
        let left: Vec<_> = left.encode_utf16().collect();
        let right: Vec<_> = right.encode_utf16().collect();
        unsafe {
            windows_sys::Win32::Globalization::CompareStringOrdinal(
                left.as_ptr(),
                left.len() as i32,
                right.as_ptr(),
                right.len() as i32,
                1,
            ) == 2
        }
    }
    fn descend(
        chain: &mut Vec<File>,
        from: usize,
        names: &[String],
        volume: u32,
        control: &Control,
    ) -> anyhow::Result<usize> {
        let mut parent = from;
        for name in names {
            control.check()?;
            let file = child(
                &chain[parent],
                name,
                FILE_TRAVERSE | FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                true,
                false,
            )?;
            ensure!(
                information(&file, true)?.dwVolumeSerialNumber == volume,
                "cross-volume directory is unsupported"
            );
            chain.push(file);
            parent = chain.len() - 1;
        }
        Ok(parent)
    }

    pub(super) struct Prepared {
        validated: Validated,
        chain: Vec<File>,
        destination_parent: usize,
        source: File,
        source_info: BY_HANDLE_FILE_INFORMATION,
    }
    impl Prepared {
        pub(super) fn prepare(validated: Validated, control: &Control) -> anyhow::Result<Self> {
            ensure!(
                !same_name(&validated.request.source, &validated.request.destination),
                "source and destination are ordinal aliases"
            );
            let drive_wide: Vec<_> = validated.drive.encode_utf16().chain(Some(0)).collect();
            ensure!(
                matches!(unsafe { GetDriveTypeW(drive_wide.as_ptr()) }, 2 | 3),
                "only local fixed/removable drives are supported"
            );
            let drive = OpenOptions::new()
                .access_mode(FILE_TRAVERSE | FILE_READ_ATTRIBUTES)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&validated.drive)?;
            let volume = information(&drive, true)?.dwVolumeSerialNumber;
            let mut chain = vec![drive];
            let root = descend(&mut chain, 0, &validated.root_parts, volume, control)?;
            let source_parent = descend(
                &mut chain,
                root,
                &validated.source[..validated.source.len() - 1],
                volume,
                control,
            )?;
            let destination_parent = descend(
                &mut chain,
                root,
                &validated.destination[..validated.destination.len() - 1],
                volume,
                control,
            )?;
            control.check()?;
            let access = FILE_READ_DATA
                | FILE_READ_ATTRIBUTES
                | if validated.request.kind == Kind::Copy {
                    0
                } else {
                    DELETE
                };
            let source = child(
                &chain[source_parent],
                validated.source.last().unwrap(),
                access,
                FILE_SHARE_READ,
                false,
                false,
            )?;
            let source_info = information(&source, false)?;
            ensure!(
                source_info.dwVolumeSerialNumber == volume,
                "source volume changed"
            );
            if validated.request.kind == Kind::Copy {
                ensure_copy_has_no_named_streams(&source)?;
                ensure!(
                    length(&source_info) <= MAX_COPY_BYTES,
                    "copy exceeds 16 MiB"
                );
            }
            control.check()?;
            Ok(Self {
                validated,
                chain,
                destination_parent,
                source,
                source_info,
            })
        }
        pub(super) fn commit(mut self, control: &Control) -> anyhow::Result<Outcome> {
            control.check()?;
            let current = information(&self.source, false)?;
            ensure!(
                identity(&current) == identity(&self.source_info)
                    && length(&current) == length(&self.source_info)
                    && current.ftLastWriteTime.dwLowDateTime
                        == self.source_info.ftLastWriteTime.dwLowDateTime
                    && current.ftLastWriteTime.dwHighDateTime
                        == self.source_info.ftLastWriteTime.dwHighDateTime,
                "prepared source changed"
            );
            // Relative child operations stay bound to these exact directory objects.
            for directory in &self.chain {
                information(directory, true)?;
            }
            let parent = &self.chain[self.destination_parent];
            let leaf = self.validated.destination.last().unwrap();
            let source_identity = identity(&self.source_info);
            let bytes = length(&self.source_info);
            let destination_identity = if self.validated.request.kind == Kind::Copy {
                let name = format!(".flowmux-copy-{}.tmp", uuid::Uuid::new_v4());
                control.check()?;
                let file = child(
                    parent,
                    &name,
                    FILE_READ_ATTRIBUTES | FILE_WRITE_DATA | DELETE,
                    FILE_SHARE_READ,
                    false,
                    true,
                )?;
                let mut temp = Temporary {
                    file,
                    published: false,
                };
                let operation = (|| -> anyhow::Result<Identity> {
                    let stamp = information(&temp.file, false)?;
                    ensure!(
                        stamp.dwVolumeSerialNumber == source_identity.volume,
                        "temporary file volume changed"
                    );
                    let mut remaining = bytes;
                    let mut buffer = [0u8; 64 * 1024];
                    while remaining != 0 {
                        control.check()?;
                        let limit = remaining.min(buffer.len() as u64) as usize;
                        let count = self.source.read(&mut buffer[..limit])?;
                        ensure!(count != 0, "source shortened during copy");
                        temp.file.write_all(&buffer[..count])?;
                        remaining -= count as u64;
                    }
                    let mut extra = [0u8; 1];
                    ensure!(
                        self.source.read(&mut extra)? == 0,
                        "source grew during copy"
                    );
                    control.check()?;
                    temp.file.sync_all()?;
                    ensure_copy_has_no_named_streams(&self.source)?;
                    control.check()?;
                    rename(&temp.file, parent, leaf)?;
                    temp.published = true; // Commit succeeded; no later cancellation rollback.
                    Ok(identity(&stamp))
                })();
                match operation {
                    Ok(id) => id,
                    Err(error) => {
                        let cleanup = temp.cleanup();
                        return match cleanup {
                            Ok(()) => Err(error),
                            Err(cleanup) => Err(error.context(format!(
                                "owned temporary cleanup failed ({name}): {cleanup:#}"
                            ))),
                        };
                    }
                }
            } else {
                control.check()?;
                rename(&self.source, parent, leaf)?;
                source_identity.clone()
            };
            Ok(Outcome {
                kind: self.validated.request.kind,
                source: self.validated.request.source,
                destination: self.validated.request.destination,
                source_identity,
                destination_identity,
                bytes,
                warnings: Vec::new(),
            })
        }
    }

    fn rename(source: &File, parent: &File, name: &str) -> anyhow::Result<()> {
        component(name)?;
        let wide: Vec<_> = name.encode_utf16().collect();
        // windows-sys 0.61.2 FILE_RENAME_INFORMATION has the identical repr(C)
        // union/handle/length/flexible UTF-16 layout as FILE_RENAME_INFO. The
        // native class is 10, not Win32 FileRenameInfo (3). Use the native call:
        // the Win32 wrapper rejected a non-null RootDirectory with error 87 on
        // the tested Windows host. Retain relative-parent confinement here.
        // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/nf-ntifs-ntsetinformationfile
        let bytes = size_of::<FILE_RENAME_INFO>()
            .checked_add(wide.len() * 2)
            .context("rename buffer overflow")?;
        let mut storage = vec![0u64; bytes.div_ceil(8)];
        let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        unsafe {
            (*info).Anonymous.ReplaceIfExists = false;
            (*info).RootDirectory = parent.as_raw_handle();
            (*info).FileNameLength = (wide.len() * 2) as u32;
            std::ptr::copy_nonoverlapping(
                wide.as_ptr(),
                std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
                wide.len(),
            );
            let mut status = IO_STATUS_BLOCK::default();
            // Both handles and the aligned buffer outlive this synchronous call.
            // The source was opened with DELETE and synchronous I/O; publication
            // keeps ReplaceIfExists false and resolves one leaf against parent.
            let result = NtSetInformationFile(
                source.as_raw_handle(),
                &mut status,
                info.cast(),
                bytes as u32,
                FILE_RENAME_INFORMATION_CLASS,
            );
            if result < 0 {
                return Err(io::Error::from_raw_os_error(
                    RtlNtStatusToDosError(result) as i32
                ))
                .context("native no-replacement rename failed");
            }
        }
        Ok(())
    }
    struct Temporary {
        file: File,
        published: bool,
    }
    impl Temporary {
        fn cleanup(&mut self) -> anyhow::Result<()> {
            if self.published {
                return Ok(());
            }
            let info = FILE_DISPOSITION_INFO { DeleteFile: true };
            if unsafe {
                SetFileInformationByHandle(
                    self.file.as_raw_handle(),
                    FileDispositionInfo,
                    (&info as *const FILE_DISPOSITION_INFO).cast(),
                    size_of::<FILE_DISPOSITION_INFO>() as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error())
                    .context("handle-owned temporary deletion failed");
            }
            self.published = true; // Deletion has been armed; Drop closes this exact handle.
            Ok(())
        }
    }
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = self.cleanup();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(source: &str, destination: &str) -> Request {
        Request {
            kind: Kind::Move,
            root: PathBuf::from("C:/한글/work"),
            source: source.into(),
            destination: destination.into(),
        }
    }
    #[test]
    fn validation_preserves_unicode_and_normalizes_separators() {
        let value = validate(request("안녕/한글😀.txt", "다음\\한글😀.txt")).unwrap();
        assert_eq!(value.request.source, "안녕/한글😀.txt");
        assert_eq!(value.request.destination, "다음/한글😀.txt");
        assert_eq!(value.drive, "C:\\");
        assert_eq!(value.root_parts, ["한글", "work"]);
    }
    #[test]
    fn invalid_destination_never_reaches_native_preparation() {
        for destination in [
            "",
            "/absolute",
            "../out",
            "a/../out",
            "a//b",
            "x:",
            "a/CON.txt",
            "LPT¹",
            "CLOCK$",
            "clock$.txt",
            "file.",
            "file ",
            "a\0b",
            "a?b",
        ] {
            assert!(
                validate(request("original", destination)).is_err(),
                "{destination:?}"
            );
        }
        for root in [
            "relative",
            "C:relative",
            "\\\\server\\share",
            "\\\\?\\C:\\root",
            "C:\\a\\..\\b",
        ] {
            let mut value = request("source", "target");
            value.root = root.into();
            assert!(validate(value).is_err(), "{root:?}");
        }
    }
    #[test]
    fn rename_scope_alias_and_cancellation_are_rejected() {
        assert!(validate(request("a/TEXT", "a/text")).is_err());
        let mut value = request("a/file", "b/new");
        value.kind = Kind::Rename;
        assert!(validate(value).is_err());
        let mut value = request("a/file", "새이름.txt");
        value.kind = Kind::Rename;
        assert_eq!(validate(value).unwrap().request.destination, "a/새이름.txt");
        let control = Control {
            cancelled: Arc::new(AtomicBool::new(true)),
            deadline: Instant::now() + std::time::Duration::from_secs(1),
        };
        assert!(control.check().is_err());
    }
    #[test]
    fn case_sensitive_and_unknown_directory_flags_fail_closed() {
        assert!(ordinary_case_flags(0).is_ok());
        for flags in [1, 2, u32::MAX] {
            assert!(ordinary_case_flags(flags).is_err());
        }
    }

    #[cfg(windows)]
    mod windows_native {
        use super::*;
        use std::{fs, path::Path, time::Duration};
        fn extended(path: &Path) -> PathBuf {
            PathBuf::from(format!(
                "\\\\?\\{}",
                path.to_str().unwrap().replace('/', "\\")
            ))
        }
        struct Fixture {
            base: PathBuf,
            root: PathBuf,
        }
        impl Fixture {
            fn new() -> Self {
                let base = std::env::temp_dir()
                    .join(format!("flowmux-file-operation-{}", uuid::Uuid::new_v4()));
                let root = (0..8).fold(base.clone(), |p, i| {
                    p.join(format!("한글{i}-{}", "long".repeat(8)))
                });
                fs::create_dir_all(extended(&root.join("원본/하위"))).unwrap();
                fs::create_dir_all(extended(&root.join("대상/하위"))).unwrap();
                Self { base, root }
            }
            fn path(&self, relative: &str) -> PathBuf {
                extended(&self.root.join(relative))
            }
            fn request(&self, kind: Kind, source: &str, destination: &str) -> Request {
                Request {
                    kind,
                    root: self.root.clone(),
                    source: source.into(),
                    destination: destination.into(),
                }
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(extended(&self.base));
            }
        }
        fn control() -> Control {
            Control {
                cancelled: Arc::new(AtomicBool::new(false)),
                deadline: Instant::now() + Duration::from_secs(10),
            }
        }

        #[test]
        fn native_long_unicode_copy_collision_and_move_preserve_bytes() {
            let fixture = Fixture::new();
            assert!(fixture.root.to_str().unwrap().encode_utf16().count() > 300);
            let source = "원본/하위/한글😀.txt";
            let copy = "대상/하위/복사😀.txt";
            let moved = "대상/하위/이동😀.txt";
            let bytes = b"\xef\xbb\xbfHangul \xed\x95\x9c\xea\xb8\x80\r\n\0binary";
            fs::write(fixture.path(source), bytes).unwrap();
            let control = control();
            let receipt = Prepared::prepare(fixture.request(Kind::Copy, source, copy), &control)
                .unwrap()
                .commit(&control)
                .unwrap();
            assert_eq!(receipt.bytes, bytes.len() as u64);
            assert_ne!(receipt.source_identity, receipt.destination_identity);
            assert_eq!(fs::read(fixture.path(source)).unwrap(), bytes);
            assert_eq!(fs::read(fixture.path(copy)).unwrap(), bytes);
            assert!(
                Prepared::prepare(fixture.request(Kind::Copy, source, copy), &control)
                    .unwrap()
                    .commit(&control)
                    .is_err()
            );
            assert_eq!(fs::read(fixture.path(copy)).unwrap(), bytes);
            assert_eq!(fs::read_dir(fixture.path("대상/하위")).unwrap().count(), 1);
            let moved_receipt =
                Prepared::prepare(fixture.request(Kind::Move, source, moved), &control)
                    .unwrap()
                    .commit(&control)
                    .unwrap();
            assert_eq!(
                moved_receipt.source_identity,
                moved_receipt.destination_identity
            );
            assert!(!fixture.path(source).exists());
            assert_eq!(fs::read(fixture.path(moved)).unwrap(), bytes);
        }

        #[test]
        fn native_copy_refuses_named_streams_without_losing_security_metadata() {
            let fixture = Fixture::new();
            let source = "원본/하위/download.txt";
            let destination = "대상/하위/copy.txt";
            let control = control();
            fs::write(fixture.path(source), b"downloaded").unwrap();
            let zone = b"[ZoneTransfer]\r\nZoneId=3\r\n";
            fs::write(fixture.path(&format!("{source}:Zone.Identifier")), zone).unwrap();
            let error =
                Prepared::prepare(fixture.request(Kind::Copy, source, destination), &control)
                    .err()
                    .expect("copy must retain or refuse named streams");
            assert!(
                error.to_string().contains("alternate data streams"),
                "{error:#}"
            );
            assert_eq!(fs::read(fixture.path(source)).unwrap(), b"downloaded");
            assert_eq!(
                fs::read(fixture.path(&format!("{source}:Zone.Identifier"))).unwrap(),
                zone
            );
            assert_eq!(fs::read_dir(fixture.path("대상/하위")).unwrap().count(), 0);

            // Rename/move retain the file object and therefore its streams.
            Prepared::prepare(fixture.request(Kind::Move, source, destination), &control)
                .unwrap()
                .commit(&control)
                .unwrap();
            assert_eq!(
                fs::read(fixture.path(&format!("{destination}:Zone.Identifier"))).unwrap(),
                zone
            );

            // Empty ordinary files remain copyable.
            fs::write(fixture.path(source), b"").unwrap();
            Prepared::prepare(
                fixture.request(Kind::Copy, source, "대상/하위/empty.txt"),
                &control,
            )
            .unwrap()
            .commit(&control)
            .unwrap();
            assert_eq!(fs::read(fixture.path("대상/하위/empty.txt")).unwrap(), b"");
        }

        #[test]
        fn native_prepared_handles_prevent_ancestor_and_source_replacement() {
            let fixture = Fixture::new();
            let source = "원본/하위/원본.txt";
            fs::write(fixture.path(source), b"original").unwrap();
            let control = control();
            let prepared = Prepared::prepare(
                fixture.request(Kind::Move, source, "대상/하위/result.txt"),
                &control,
            )
            .unwrap();
            assert!(fs::rename(fixture.path("대상"), fixture.path("다른대상")).is_err());
            assert!(fs::rename(fixture.path("원본"), fixture.path("다른원본")).is_err());
            assert!(fs::rename(fixture.path(source), fixture.path("replacement.txt")).is_err());
            assert!(fs::write(fixture.path(source), b"replacement").is_err());
            assert_eq!(fs::read(fixture.path(source)).unwrap(), b"original");
            drop(prepared);
            fs::rename(fixture.path("대상"), fixture.path("다른대상")).unwrap();
            assert!(!fixture.path("대상/하위/result.txt").exists());
        }

        #[test]
        fn native_precommit_cancellation_does_not_mutate_and_sharing_conflicts_fail() {
            use std::os::windows::fs::OpenOptionsExt;
            let fixture = Fixture::new();
            let source = "원본/하위/원본.txt";
            fs::write(fixture.path(source), b"original").unwrap();
            let control = control();
            let prepared = Prepared::prepare(
                fixture.request(Kind::Copy, source, "대상/하위/cancelled.txt"),
                &control,
            )
            .unwrap();
            control.cancelled.store(true, Ordering::Release);
            assert!(prepared.commit(&control).is_err());
            assert_eq!(fs::read_dir(fixture.path("대상/하위")).unwrap().count(), 0);
            assert_eq!(fs::read(fixture.path(source)).unwrap(), b"original");
            control.cancelled.store(false, Ordering::Release);
            let writer = fs::OpenOptions::new()
                .write(true)
                .share_mode(7)
                .open(fixture.path(source))
                .unwrap();
            assert!(Prepared::prepare(
                fixture.request(Kind::Copy, source, "대상/하위/rejected.txt"),
                &control
            )
            .is_err());
            drop(writer);
            Prepared::prepare(
                fixture.request(Kind::Rename, source, "원본/하위/새이름.txt"),
                &control,
            )
            .unwrap()
            .commit(&control)
            .unwrap();
            assert_eq!(
                fs::read(fixture.path("원본/하위/새이름.txt")).unwrap(),
                b"original"
            );
        }
    }
}
