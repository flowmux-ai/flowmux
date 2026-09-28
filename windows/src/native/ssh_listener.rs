// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::{bail, ensure};
use std::mem::{offset_of, size_of};
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR},
    NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
        MIB_TCP_STATE_LISTEN, TCP_TABLE_OWNER_PID_LISTENER,
    },
    Networking::WinSock::AF_INET,
};

/// This is a listener snapshot, not a process-liveness or authentication check.
/// The caller must still retain the matching Session and reject exited epochs.
pub(super) fn owns_listener(pid: u32, port: u16) -> anyhow::Result<bool> {
    ensure!(pid != 0 && port != 0, "Listener PID and port must not be zero");
    const MAX_TABLE_BYTES: usize = 16 * 1024 * 1024;
    let mut storage: Vec<usize> = Vec::new();
    for _ in 0..4 {
        let capacity = storage.len() * size_of::<usize>();
        let mut bytes = capacity as u32;
        let result = unsafe {
            GetExtendedTcpTable(
                if storage.is_empty() {
                    std::ptr::null_mut()
                } else {
                    storage.as_mut_ptr().cast()
                },
                &mut bytes,
                0,
                u32::from(AF_INET),
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if result == ERROR_INSUFFICIENT_BUFFER {
            ensure!(
                bytes as usize > capacity && bytes as usize <= MAX_TABLE_BYTES,
                "Invalid or oversized TCP listener table"
            );
            storage.resize((bytes as usize).div_ceil(size_of::<usize>()), 0);
            continue;
        }
        ensure!(result == NO_ERROR, "GetExtendedTcpTable failed ({result})");
        let offset = offset_of!(MIB_TCPTABLE_OWNER_PID, table);
        ensure!(
            bytes as usize >= offset && bytes as usize <= capacity,
            "Truncated TCP listener table"
        );
        // usize storage supplies the alignment required by the Win32 DWORD rows.
        let count = unsafe { storage.as_ptr().cast::<u32>().read() } as usize;
        ensure!(
            count <= (bytes as usize - offset) / size_of::<MIB_TCPROW_OWNER_PID>(),
            "TCP listener count exceeds the returned buffer"
        );
        let rows = unsafe {
            std::slice::from_raw_parts(
                storage
                    .as_ptr()
                    .cast::<u8>()
                    .add(offset)
                    .cast::<MIB_TCPROW_OWNER_PID>(),
                count,
            )
        };
        return Ok(rows.iter().any(|row| {
            row.dwOwningPid == pid
                && row.dwState == MIB_TCP_STATE_LISTEN as u32
                && row.dwLocalAddr.to_ne_bytes() == [127, 0, 0, 1]
                && u16::from_be(row.dwLocalPort as u16) == port
        }));
    }
    bail!("TCP listener table kept changing during the bounded query")
}
