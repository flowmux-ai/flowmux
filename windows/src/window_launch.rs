// SPDX-License-Identifier: GPL-3.0-or-later
//! Lossless Windows launch context. Environment values must never be logged.
use serde::{Deserialize, Serialize};

pub const MAX_ENV_UNITS: usize = 128 * 1024;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchContext {
    pub arguments: Vec<Vec<u16>>,
    pub directory: Vec<u16>,
    pub environment: Vec<u16>,
}
impl std::fmt::Debug for LaunchContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaunchContext")
            .field("argument_count", &self.arguments.len())
            .field("directory_units", &self.directory.len())
            .field("environment_units", &self.environment.len())
            .finish()
    }
}
impl LaunchContext {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.arguments.len() <= 256, "too many launch arguments");
        anyhow::ensure!(
            self.arguments.iter().map(Vec::len).sum::<usize>() < 32767
                && self.arguments.iter().all(|a| !a.contains(&0)),
            "launch arguments contain NUL or exceed the Windows limit"
        );
        anyhow::ensure!(
            !self.directory.is_empty()
                && self.directory.len() < 32767
                && !self.directory.contains(&0),
            "invalid launch directory"
        );
        anyhow::ensure!(
            self.environment.len() <= MAX_ENV_UNITS && self.environment.ends_with(&[0, 0]),
            "invalid or oversized launch environment"
        );
        let body = &self.environment[..self.environment.len() - 2];
        if !body.is_empty() {
            for entry in body.split(|c| *c == 0) {
                anyhow::ensure!(
                    entry.len() >= 2 && entry.iter().skip(1).any(|c| *c == 61),
                    "malformed launch environment entry"
                );
            }
        }
        Ok(())
    }

    /// Retain every other UTF-16 unit, including drive-directory entries, while
    /// removing the old terminal's routing context. New PTYs receive fresh IDs.
    pub fn detached_environment(&self) -> anyhow::Result<Vec<u16>> {
        self.validate()?;
        let mut out = Vec::new();
        for entry in self.environment[..self.environment.len() - 2].split(|c| *c == 0) {
            if entry.is_empty() {
                continue;
            }
            let key_end = entry.iter().position(|c| *c == 61).unwrap_or(0);
            let key: Vec<_> = entry[..key_end]
                .iter()
                .map(|c| if (97..=122).contains(c) { c - 32 } else { *c })
                .collect();
            if [
                "FLOWMUX_PIPE_NAME",
                "FLOWMUX_SOCKET_PATH",
                "FLOWMUX_SURFACE_ID",
                "FLOWMUX_PANE_ID",
                "FLOWMUX_WORKSPACE_ID",
                "FLOWMUX_TAB_ID",
                "FLOWMUX_BUNDLED_CLI_PATH",
            ]
            .iter()
            .any(|name| name.encode_utf16().eq(key.iter().copied()))
            {
                continue;
            }
            out.extend_from_slice(entry);
            out.push(0);
        }
        if out.is_empty() {
            out.push(0);
        }
        out.push(0);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> LaunchContext {
        LaunchContext { arguments: vec![], directory: "C:\\한글".encode_utf16().collect(),
            environment: "=C:=C:\\한글\0Path=C:\\bin\0flowmux_pipe_name=old\0FLOWMUX_SURFACE_ID=old\0USER_SECRET=한글 한 😀\nvalue\0\0".encode_utf16().collect() }
    }
    #[test]
    fn environment_removes_only_terminal_context_without_normalizing_or_logging_values() {
        let mut value = context();
        assert_eq!(
            String::from_utf16(&value.detached_environment().unwrap()).unwrap(),
            "=C:=C:\\한글\0Path=C:\\bin\0USER_SECRET=한글 한 😀\nvalue\0\0"
        );
        assert!(!format!("{value:?}").contains("USER_SECRET"));
        value.environment = vec![65, 61, 0xd800, 0, 0];
        assert_eq!(value.detached_environment().unwrap(), value.environment);
        value.environment = vec![0, 0];
        assert_eq!(value.detached_environment().unwrap(), vec![0, 0]);
    }
    #[test]
    fn launch_context_rejects_malformed_or_oversized_inputs_before_process_creation() {
        for environment in [
            vec![],
            vec![65, 61, 66, 0],
            vec![65, 0, 0],
            vec![0; MAX_ENV_UNITS + 1],
        ] {
            let mut value = context();
            value.environment = environment;
            assert!(value.validate().is_err());
        }
        let mut value = context();
        value.arguments = vec![vec![0]];
        assert!(value.validate().is_err());
        value.arguments = vec![vec![]; 257];
        assert!(value.validate().is_err());
        value.arguments = vec![vec![65; 32767]];
        assert!(value.validate().is_err());
        value.arguments = vec![];
        value.directory = vec![0];
        assert!(value.validate().is_err());
    }
}
