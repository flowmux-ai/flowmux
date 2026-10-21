// SPDX-License-Identifier: GPL-3.0-or-later
//! Native process inspection. No procfs assumptions escape this backend.
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

/// The kernel changes this UUID at boot, independently of wall-clock changes.
pub fn boot_session_id() -> Option<String> {
    let mut bytes = [0_u8; 37];
    let mut size = bytes.len();
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 || size != bytes.len() {
        return None;
    }
    std::ffi::CStr::from_bytes_with_nul(&bytes)
        .ok()?
        .to_str()
        .ok()
        .map(str::to_owned)
}

pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    if pid == 0 || pid > i32::MAX as u32 {
        return None;
    }
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int,
        )
    };
    if rc as usize != std::mem::size_of::<libc::proc_vnodepathinfo>() {
        return None;
    }

    let bytes = unsafe {
        std::slice::from_raw_parts(
            info.pvi_cdir.vip_path.as_ptr() as *const u8,
            std::mem::size_of_val(&info.pvi_cdir.vip_path),
        )
    };
    let len = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    if len == 0 {
        return None;
    }

    Some(PathBuf::from(OsString::from_vec(bytes[..len].to_vec())))
}

pub(super) fn procargs(pid: u32) -> std::io::Result<Vec<u8>> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let mut argmax_mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let mut argmax = 0_i32;
    let mut argmax_size = std::mem::size_of_val(&argmax);
    let rc = unsafe {
        libc::sysctl(
            argmax_mib.as_mut_ptr(),
            argmax_mib.len() as libc::c_uint,
            (&mut argmax as *mut i32).cast(),
            &mut argmax_size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || argmax <= 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut raw = vec![0_u8; argmax as usize];
    let mut raw_size = raw.len();
    let mut procargs_mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
    let rc = unsafe {
        libc::sysctl(
            procargs_mib.as_mut_ptr(),
            procargs_mib.len() as libc::c_uint,
            raw.as_mut_ptr().cast(),
            &mut raw_size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    raw.truncate(raw_size);
    Ok(raw)
}

// KERN_PROCARGS2: argc, executable path, NUL padding, argc strings, environment.
pub(super) fn procargs_slices(raw: &[u8]) -> std::io::Result<(Vec<&[u8]>, &[u8])> {
    let invalid = || std::io::Error::from(std::io::ErrorKind::InvalidData);
    let argc = i32::from_ne_bytes(raw.get(..4).ok_or_else(invalid)?.try_into().unwrap());
    if argc <= 0 || argc as usize > raw.len() {
        return Err(invalid());
    }
    let mut rest = &raw[4..];
    let executable_end = rest
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(invalid)?;
    rest = &rest[executable_end..];
    let padding_end = rest
        .iter()
        .position(|byte| *byte != 0)
        .ok_or_else(invalid)?;
    rest = &rest[padding_end..];
    let mut argv = Vec::with_capacity(argc as usize);
    for _ in 0..argc {
        let end = rest
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(invalid)?;
        argv.push(&rest[..end]);
        rest = &rest[end + 1..];
    }
    Ok((argv, rest))
}

/// Read only the requested launch environment keys from a live process.
pub fn process_environment(
    pid: u32,
    keys: &[&str],
) -> std::io::Result<std::collections::BTreeMap<String, String>> {
    environment_from_procargs(&procargs(pid)?, keys)
}

fn environment_from_procargs(
    raw: &[u8],
    keys: &[&str],
) -> std::io::Result<std::collections::BTreeMap<String, String>> {
    let (_, environment) = procargs_slices(raw)?;
    let mut selected = std::collections::BTreeMap::new();
    for entry in environment
        .split(|byte| *byte == 0)
        .take_while(|entry| !entry.is_empty())
    {
        let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let Some(key) = keys
            .iter()
            .find(|key| key.as_bytes() == &entry[..separator])
        else {
            continue;
        };
        let value = &entry[separator + 1..];
        if !value.is_empty() {
            let value = std::str::from_utf8(value).map_err(|_| std::io::ErrorKind::InvalidData)?;
            selected
                .entry((*key).to_string())
                .or_insert_with(|| value.to_string());
        }
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    #[test]
    fn environment_skips_arguments_and_filters_requested_keys() {
        let mut raw = 3_i32.to_ne_bytes().to_vec();
        raw.extend_from_slice(b"/usr/bin/node\0\0node\0codex.js\0CODEX_HOME=argument\0HOME=/home/test\0CODEX_HOME=/custom path\0SECRET=hidden\0\0");
        let env = environment_from_procargs(&raw, &["CODEX_HOME", "HOME"]).unwrap();
        assert_eq!(env.len(), 2);
        assert_eq!(env["CODEX_HOME"], "/custom path");
        assert_eq!(env["HOME"], "/home/test");
    }

    #[test]
    fn environment_rejects_truncated_arguments_and_invalid_selected_values() {
        for raw in [vec![], vec![0, 0, 0, 0], 100_i32.to_ne_bytes().to_vec()] {
            assert!(environment_from_procargs(&raw, &["HOME"]).is_err());
        }
        let mut raw = 1_i32.to_ne_bytes().to_vec();
        raw.extend_from_slice(b"/bin/sh\0\0sh\0HOME=\xff\0OTHER=ok\0");
        assert!(environment_from_procargs(&raw, &["HOME"]).is_err());
        assert_eq!(
            environment_from_procargs(&raw, &["OTHER"]).unwrap()["OTHER"],
            "ok"
        );
    }

    #[test]
    fn cwd_tracks_a_live_child_without_shell_integration() {
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                "printf 'ready\\n'; read next; cd \"$next\"; printf 'changed\\n'; read done",
            ])
            .current_dir("/tmp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        let initial = process_cwd(child.id());
        writeln!(child.stdin.as_mut().unwrap(), "/System/Library").unwrap();
        line.clear();
        output.read_line(&mut line).unwrap();
        let changed = process_cwd(child.id());
        writeln!(child.stdin.as_mut().unwrap(), "done").unwrap();
        child.wait().unwrap();
        assert_eq!(initial, Some(std::fs::canonicalize("/tmp").unwrap()));
        assert_eq!(changed, Some(PathBuf::from("/System/Library")));
        assert_eq!(process_cwd(child.id()), None);
    }

    #[test]
    fn cwd_rejects_invalid_pids() {
        assert_eq!(process_cwd(0), None);
        assert_eq!(process_cwd(u32::MAX), None);
    }
}
