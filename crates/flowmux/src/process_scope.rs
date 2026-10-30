// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux user-manager scopes. The caller holds the pre-exec gate until attachment.

use flowmux_core::SurfaceId;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zbus::{OwnedObjectPath, Variant};

#[derive(Clone)]
pub enum ScopeIsolation {
    Available {
        conn: zbus::Connection,
        slice: Option<String>,
        own_unit: Option<String>,
    },
    Unavailable {
        reason: String,
    },
}

impl ScopeIsolation {
    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Self::Unavailable { reason } => Some(reason),
            _ => None,
        }
    }

    async fn manager(&self) -> Result<zbus::Proxy<'_>, String> {
        let Self::Available { conn, .. } = self else {
            return Err(self.unavailable_reason().unwrap_or("unavailable").into());
        };
        zbus::Proxy::new(
            conn,
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
        )
        .await
        .map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneScope {
    pub unit: String,
    pub cgroup_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScopeLimits {
    pub memory_high: Option<u64>,
    pub memory_max: Option<u64>,
}

pub async fn probe(enabled: bool) -> ScopeIsolation {
    async fn available(enabled: bool) -> Result<ScopeIsolation, String> {
        if !enabled {
            return Err("disabled by process_isolation = off".into());
        }
        if crate::ui::ghostty_pane::is_flatpak_sandbox() {
            return Err("Flatpak sandbox".into());
        }
        if !Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
            return Err("cgroup v2 is not mounted".into());
        }
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").map_err(|e| e.to_string())?;
        let own = cgroup
            .lines()
            .find_map(|s| s.strip_prefix("0::"))
            .ok_or("own cgroup unknown")?;
        let path = Path::new(own);
        let slice = path
            .ancestors()
            .filter_map(|p| p.file_name()?.to_str())
            .find(|s| s.ends_with(".slice"))
            .map(str::to_owned);
        let own_unit = path
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| s.ends_with(".scope") || s.ends_with(".service"))
            .map(str::to_owned);
        let conn = zbus::Connection::session()
            .await
            .map_err(|e| e.to_string())?;
        let iso = ScopeIsolation::Available {
            conn,
            slice,
            own_unit,
        };
        let _: String = iso
            .manager()
            .await?
            .get_property("Version")
            .await
            .map_err(|e| e.to_string())?;
        Ok(iso)
    }
    match tokio::time::timeout(Duration::from_secs(1), available(enabled)).await {
        Ok(Ok(iso)) => iso,
        Ok(Err(reason)) => ScopeIsolation::Unavailable { reason },
        Err(_) => ScopeIsolation::Unavailable {
            reason: "user service manager probe timed out".into(),
        },
    }
}

pub fn unit_name(gui_pid: u32, surface: SurfaceId) -> String {
    format!("flowmux-{gui_pid}-{surface}.scope")
}

pub async fn start_scope(
    iso: &ScopeIsolation,
    unit: &str,
    description: &str,
    child_pid: u32,
    limits: ScopeLimits,
) -> Result<PaneScope, String> {
    let manager = iso.manager().await?;
    let mut props = vec![
        ("PIDs", Variant::from(vec![child_pid])),
        ("Description", Variant::from(description)),
        ("OOMPolicy", Variant::from("stop")),
        ("CollectMode", Variant::from("inactive")),
    ];
    if let ScopeIsolation::Available {
        slice: Some(slice), ..
    } = iso
    {
        props.push(("Slice", Variant::from(slice.as_str())));
    }
    if let Some(value) = limits.memory_high {
        props.push(("MemoryHigh", Variant::from(value)));
    }
    if let Some(value) = limits.memory_max {
        props.push(("MemoryMax", Variant::from(value)));
        props.push(("MemorySwapMax", Variant::from(0u64)));
    }
    let aux: Vec<(&str, Vec<(&str, Variant<'_>)>)> = vec![];
    let _: OwnedObjectPath = manager
        .call("StartTransientUnit", &(unit, "fail", props, aux))
        .await
        .map_err(|e| format!("StartTransientUnit: {e}"))?;
    // A job receipt is not attachment: keep the child parked until its cgroup moved.
    let deadline = tokio::time::Instant::now() + Duration::from_millis(800);
    loop {
        let cgroup = std::fs::read_to_string(format!("/proc/{child_pid}/cgroup"))
            .map_err(|e| format!("child cgroup: {e}"))?;
        if let Some(path) = cgroup
            .lines()
            .find_map(|s| s.strip_prefix("0::"))
            .filter(|s| Path::new(s).file_name().is_some_and(|s| s == unit))
        {
            return Ok(PaneScope {
                unit: unit.into(),
                cgroup_dir: Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/')),
            });
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("scope attachment timed out".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub async fn scope_result(iso: &ScopeIsolation, unit: &str) -> Result<String, String> {
    let manager = iso.manager().await?;
    let path: OwnedObjectPath = manager
        .call("GetUnit", &(unit,))
        .await
        .map_err(|e| format!("GetUnit: {e}"))?;
    let ScopeIsolation::Available { conn, .. } = iso else {
        unreachable!()
    };
    let proxy = zbus::Proxy::new(
        conn,
        "org.freedesktop.systemd1",
        path,
        "org.freedesktop.systemd1.Scope",
    )
    .await
    .map_err(|e| e.to_string())?;
    proxy
        .get_property("Result")
        .await
        .map_err(|e| format!("Scope.Result: {e}"))
}

pub async fn reset_failed(iso: &ScopeIsolation, unit: &str) -> Result<(), String> {
    iso.manager()
        .await?
        .call("ResetFailedUnit", &(unit,))
        .await
        .map_err(|e| format!("ResetFailedUnit: {e}"))
}

type Unit = (
    String,
    String,
    String,
    String,
    String,
    String,
    OwnedObjectPath,
    u32,
    String,
    OwnedObjectPath,
);

pub async fn sweep_failed_scopes(iso: &ScopeIsolation) -> Result<usize, String> {
    let units: Vec<Unit> = iso
        .manager()
        .await?
        .call(
            "ListUnitsByPatterns",
            &(vec!["failed"], vec!["flowmux-*.scope"]),
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut count = 0;
    for unit in units {
        // Other windows may still be collecting OOM evidence from their failed scopes.
        if stale_scope(&unit.0) {
            reset_failed(iso, &unit.0).await?;
            count += 1;
        }
    }
    Ok(count)
}

fn stale_scope(unit: &str) -> bool {
    let Some(pid) = unit
        .strip_prefix("flowmux-")
        .and_then(|s| s.split('-').next())
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|pid| *pid > 0)
    else {
        return false;
    };
    !flowmux_procmon::pid_alive(pid as u32)
}

pub async fn avoid_managed_oom_for_self(iso: &ScopeIsolation) -> Result<(), String> {
    if let ScopeIsolation::Available {
        own_unit: Some(unit),
        ..
    } = iso
    {
        iso.manager()
            .await?
            .call::<_, _, ()>(
                "SetUnitProperties",
                &(
                    unit,
                    true,
                    vec![("ManagedOOMPreference", Variant::from("avoid"))],
                ),
            )
            .await
            .map_err(|e| format!("SetUnitProperties: {e}"))?;
    }
    Ok(())
}

pub fn cgroup_oom_kills(dir: &Path) -> Option<u64> {
    std::fs::read_to_string(dir.join("memory.events"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("oom_kill ")?.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names_and_stale_scope_ownership() {
        let surface = SurfaceId::new();
        assert_eq!(
            unit_name(4242, surface),
            format!("flowmux-4242-{surface}.scope")
        );
        assert!(!stale_scope(&unit_name(std::process::id(), surface)));
        assert!(!stale_scope("flowmux-invalid.scope"));
        assert!(!stale_scope("flowmux-0-x.scope"));
    }

    #[test]
    fn cgroup_events_parse_only_oom_kills() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("memory.events"),
            "oom 4\noom_kill 2\noom_group_kill 0\n",
        )
        .unwrap();
        assert_eq!(cgroup_oom_kills(dir.path()), Some(2));
        assert_eq!(cgroup_oom_kills(&dir.path().join("missing")), None);
    }

    #[tokio::test]
    async fn disabled_probe_needs_no_bus() {
        assert_eq!(
            probe(false).await.unavailable_reason(),
            Some("disabled by process_isolation = off")
        );
    }

    async fn available() -> Option<ScopeIsolation> {
        let iso = probe(true).await;
        if let Some(reason) = iso.unavailable_reason() {
            eprintln!("skipped systemd test: {reason}");
            None
        } else {
            Some(iso)
        }
    }

    #[tokio::test]
    async fn gated_child_is_attached_before_exec_and_keeps_inherited_score() {
        let Some(iso) = available().await else { return };
        let (pty, gate) =
            flowmux_terminal::pty::Pty::spawn_gated(&["sleep", "30"], None, &[], 80, 24, None)
                .unwrap();
        let pid = pty.child_pid();
        let unit = unit_name(std::process::id(), SurfaceId::new());
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            start_scope(
                &iso,
                &unit,
                "Flowmux scope test",
                pid as u32,
                ScopeLimits::default(),
            ),
        )
        .await;
        if !matches!(result, Ok(Ok(_))) {
            pty.close_async();
            panic!("scope failed: {result:?}");
        }
        let scope = result.unwrap().unwrap();
        assert!(scope.cgroup_dir.join("memory.events").exists());
        assert_eq!(
            std::fs::read_to_string(format!("/proc/{pid}/oom_score_adj")).unwrap(),
            std::fs::read_to_string("/proc/self/oom_score_adj").unwrap()
        );
        gate.release();
        pty.close_async();
    }

    #[tokio::test]
    async fn bounded_oom_preserves_evidence_until_reset() {
        let Some(iso) = available().await else { return };
        let (mut pty, gate) = flowmux_terminal::pty::Pty::spawn_gated(
            &["python3", "-c", "a=bytearray(128*1024*1024)"],
            None,
            &[],
            80,
            24,
            None,
        )
        .unwrap();
        let pid = pty.child_pid();
        let unit = unit_name(std::process::id(), SurfaceId::new());
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            start_scope(
                &iso,
                &unit,
                "Flowmux bounded OOM test",
                pid as u32,
                ScopeLimits {
                    memory_high: None,
                    memory_max: Some(64 << 20),
                },
            ),
        )
        .await;
        if !matches!(result, Ok(Ok(_))) {
            pty.close_async();
            panic!("scope failed: {result:?}");
        }
        gate.release();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if scope_result(&iso, &unit).await.as_deref() == Ok("oom-kill") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("scope never reported oom-kill");
        let mut status = 0;
        // SAFETY: own unreaped child, killed by the bounded OOM above.
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        pty.mark_child_exited();
        assert!(libc::WIFSIGNALED(status));
        assert_eq!(scope_result(&iso, &unit).await.unwrap(), "oom-kill");
        // Never sweep live windows in a test: reset only the unit we created.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                reset_failed(&iso, &unit).await.ok();
                if scope_result(&iso, &unit).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("failed unit did not unload");
    }
}
