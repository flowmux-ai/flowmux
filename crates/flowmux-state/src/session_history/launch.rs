// SPDX-License-Identifier: GPL-3.0-or-later
//! Native storage overrides shared by session browsing and window restoration.

use super::SessionAgent;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLaunchContext {
    pub environment: BTreeMap<String, String>,
    pub arguments: Vec<String>,
}

impl SessionLaunchContext {
    pub(crate) fn validate_for_resume(&self, agent: SessionAgent) -> Result<(), String> {
        if !matches!(agent, SessionAgent::Gemini | SessionAgent::Cline)
            || self
                .environment
                .keys()
                .any(|key| !agent.home_variables().contains(&key.as_str()))
            || self
                .environment
                .values()
                .chain(&self.arguments)
                .any(|value| value.chars().any(char::is_control))
            || !self.arguments.chunks(2).all(|pair| {
                agent == SessionAgent::Cline
                    && pair.len() == 2
                    && matches!(pair[0].as_str(), "--config" | "--data-dir")
                    && !pair[1].is_empty()
            })
        {
            return Err("Invalid agent storage overrides".into());
        }
        Ok(())
    }

    pub fn read(agent: SessionAgent, pid: u32) -> Result<Self, String> {
        let keys = agent
            .home_variables()
            .iter()
            .copied()
            .chain(["PATH"])
            .collect::<Vec<_>>();
        #[cfg(target_os = "linux")]
        let mut environment = {
            let bytes = std::fs::read(format!("/proc/{pid}/environ")).map_err(|e| e.to_string())?;
            let mut environment = BTreeMap::new();
            for key in &keys {
                let prefix = format!("{key}=");
                if let Some(value) = bytes
                    .split(|b| *b == 0)
                    .find_map(|entry| entry.strip_prefix(prefix.as_bytes()))
                    .filter(|v| !v.is_empty())
                {
                    environment.insert(
                        (*key).to_string(),
                        String::from_utf8(value.to_vec()).map_err(|e| e.to_string())?,
                    );
                }
            }
            environment
        };
        #[cfg(target_os = "macos")]
        let mut environment =
            flowmux_procmon::process_environment(pid, &keys).map_err(|e| e.to_string())?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let environment = agent
            .home_variables()
            .iter()
            .copied()
            .filter_map(|key| {
                std::env::var(key)
                    .ok()
                    .filter(|value| !value.is_empty())
                    .map(|value| (key.to_string(), value))
            })
            .collect();
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let arguments = {
            let _ = (pid, keys);
            Vec::new()
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let arguments = if agent == SessionAgent::Cline {
            let argv = flowmux_procmon::cmdline_of(pid);
            if argv.is_empty() {
                return Err("Agent command line is unavailable".into());
            }
            cline_arguments(
                &mut environment,
                &argv.iter().map(String::as_str).collect::<Vec<_>>(),
                || {
                    flowmux_procmon::process_cwd(pid)
                        .ok_or_else(|| "Agent working directory is unavailable".into())
                },
            )?
        } else {
            Vec::new()
        };
        Ok(Self {
            environment,
            arguments,
        })
    }
}

fn cline_arguments(
    environment: &mut std::collections::BTreeMap<String, String>,
    argv: &[&str],
    cwd: impl Fn() -> Result<PathBuf, String>,
) -> Result<Vec<String>, String> {
    let mut arguments = Vec::new();
    for (flag, variable) in [("--config", "CLINE_DIR"), ("--data-dir", "CLINE_DATA_DIR")] {
        if let Some(value) = argv.iter().enumerate().find_map(|(i, arg)| {
            arg.strip_prefix(&format!("{flag}="))
                .or_else(|| (*arg == flag).then(|| argv.get(i + 1).copied()).flatten())
        }) {
            let path = PathBuf::from(value);
            let path = if path.is_absolute() {
                path
            } else {
                cwd()?.join(path)
            };
            let value = path
                .to_str()
                .ok_or("Agent directory is not UTF-8")?
                .to_string();
            environment.insert(variable.into(), value.clone());
            if flag == "--data-dir" {
                environment.insert(
                    "CLINE_DB_DATA_DIR".into(),
                    path.join("db").to_string_lossy().into_owned(),
                );
            }
            arguments.extend([flag.to_string(), value]);
        }
    }
    Ok(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cline_options_preserve_absolute_paths_and_override_environment() {
        let mut environment = [("CLINE_DATA_DIR".into(), "/old".into())].into();
        let arguments = cline_arguments(
            &mut environment,
            &["cline", "--data-dir=/custom data", "--config", "/config"],
            || panic!("absolute options do not require a running process"),
        )
        .unwrap();
        assert_eq!(
            arguments,
            ["--config", "/config", "--data-dir", "/custom data"]
        );
        assert_eq!(environment["CLINE_DATA_DIR"], "/custom data");
        assert_eq!(environment["CLINE_DB_DATA_DIR"], "/custom data/db");
        let previous = environment.clone();
        assert!(
            cline_arguments(&mut environment, &["cline", "--config"], || Err(
                "unavailable".into()
            ))
            .unwrap()
            .is_empty()
        );
        assert_eq!(environment, previous);
        assert!(
            cline_arguments(&mut environment, &["cline", "--config=relative"], || Err(
                "unavailable".into()
            ))
            .is_err()
        );
    }
}
