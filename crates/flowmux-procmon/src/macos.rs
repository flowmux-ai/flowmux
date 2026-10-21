// SPDX-License-Identifier: GPL-3.0-or-later
//! Native process inspection. No procfs assumptions escape this backend.
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

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
