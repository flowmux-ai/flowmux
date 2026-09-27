// SPDX-License-Identifier: GPL-3.0-or-later
//! Resolve before CreateProcessW; never let a cwd file shadow a built-in profile.
use crate::shell::{quote_arg, Shell};
use anyhow::Context;
use base64::Engine;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

pub struct Resolved {
    pub executable: PathBuf,
    pub command: String,
    pub cmd_prompt: bool,
}
fn executable(path: PathBuf) -> anyhow::Result<PathBuf> {
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    anyhow::ensure!(
        ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com"),
        "shell must be an .exe or .com; invoke a script through its interpreter explicitly"
    );
    anyhow::ensure!(
        path.is_file(),
        "shell executable is unavailable: {}",
        path.display()
    );
    Ok(path)
}
fn path_search(
    name: &str,
    path: Option<OsString>,
    pathext: Option<OsString>,
) -> anyhow::Result<PathBuf> {
    let extensions: Vec<String> = if Path::new(name).extension().is_some() {
        vec![String::new()]
    } else {
        pathext
            .unwrap_or_else(|| ".COM;.EXE".into())
            .to_string_lossy()
            .split(';')
            .filter(|e| e.eq_ignore_ascii_case(".exe") || e.eq_ignore_ascii_case(".com"))
            .map(String::from)
            .collect()
    };
    if let Some(path) = path {
        for dir in std::env::split_paths(&path).filter(|p| p.is_absolute()) {
            for ext in &extensions {
                let candidate = dir.join(format!("{name}{ext}"));
                if candidate.is_file() {
                    return executable(candidate);
                }
            }
        }
    }
    anyhow::bail!("shell executable {name} was not found on the host PATH")
}
pub fn resolve(shell: &Shell) -> anyhow::Result<Resolved> {
    shell.validate()?;
    let profile = shell.program.to_ascii_lowercase();
    let system = || -> anyhow::Result<PathBuf> {
        Ok(
            PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot unavailable")?)
                .join("System32"),
        )
    };
    let path = match profile.as_str() {
        "powershell" => executable(system()?.join("WindowsPowerShell/v1.0/powershell.exe"))?,
        "cmd" => executable(system()?.join("cmd.exe"))?,
        "pwsh" => {
            let found = path_search(
                "pwsh.exe",
                std::env::var_os("PATH"),
                std::env::var_os("PATHEXT"),
            );
            match found {
                Ok(path) => path,
                Err(error) => match std::env::var_os("ProgramFiles")
                    .map(PathBuf::from)
                    .map(|p| p.join("PowerShell/7/pwsh.exe"))
                    .filter(|p| p.is_file())
                {
                    Some(path) => executable(path)?,
                    None => {
                        return Err(error.context("PowerShell 7 is not installed or discoverable"))
                    }
                },
            }
        }
        _ => {
            let path = PathBuf::from(&shell.program);
            if path.is_absolute() {
                executable(path)?
            } else {
                anyhow::ensure!(
                    !shell.program.contains(['/', '\\', ':']),
                    "use an absolute shell path or a bare PATH executable name"
                );
                path_search(
                    &shell.program,
                    std::env::var_os("PATH"),
                    std::env::var_os("PATHEXT"),
                )?
            }
        }
    };
    let mut args = shell.args.clone();
    if matches!(profile.as_str(), "powershell" | "pwsh") {
        // Fixed source only; user argv and cwd are never interpolated into it.
        let script: Vec<u8> = include_str!("../../shell/powershell.ps1")
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        args.extend([
            "-NoLogo".into(),
            "-NoExit".into(),
            "-EncodedCommand".into(),
            base64::engine::general_purpose::STANDARD.encode(script),
        ]);
    } else if profile == "cmd" {
        args.insert(0, "/D".into());
    }
    let mut command = quote_arg(&path.to_string_lossy());
    for arg in args {
        command.push(' ');
        command.push_str(&quote_arg(&arg));
    }
    anyhow::ensure!(
        command.encode_utf16().count() < 32767,
        "expanded shell command exceeds Windows command-line limit"
    );
    Ok(Resolved {
        executable: path,
        command,
        cmd_prompt: profile == "cmd",
    })
}
pub fn profiles() -> serde_json::Value {
    serde_json::json!(["powershell", "cmd", "pwsh"].map(|name| {
        match resolve(&Shell::profile(name)) {
            Ok(found) => {
                serde_json::json!({"program":name,"available":true,"executable":found.executable})
            }
            Err(error) => {
                serde_json::json!({"program":name,"available":false,"reason":format!("{error:#}")})
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn path_resolution_respects_order_extensions_and_refuses_relative_or_scripts() {
        let dir = std::env::temp_dir().join(format!("flowmux-shell-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("한글 한 😀")).unwrap();
        let sub = dir.join("한글 한 😀");
        for file in [
            dir.join("probe.exe"),
            sub.join("probe.com"),
            sub.join("probe.cmd"),
        ] {
            std::fs::write(file, []).unwrap();
        }
        let path = std::env::join_paths([sub.clone(), dir.clone()]).unwrap();
        assert_eq!(
            path_search("probe", Some(path), Some(".CMD;.COM;.EXE".into())).unwrap(),
            sub.join("probe.COM")
        );
        assert!(resolve(&Shell::profile(".\\probe.exe")).is_err());
        assert!(executable(sub.join("probe.cmd")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
