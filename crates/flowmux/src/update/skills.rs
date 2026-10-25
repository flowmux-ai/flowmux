// SPDX-License-Identifier: GPL-3.0-or-later
//! Refresh installed skills once per app version, using the new app's payload.
use super::check::Version;
use anyhow::Context;
use flowmux_cli::agent::{self, Skill, SkillOverrides};
use std::path::Path;

pub(crate) fn refresh_installed() -> anyhow::Result<()> {
    let path = flowmux_config::paths::state_dir()
        .context("state directory is unavailable")?
        .join("installed-skill-version");
    refresh(
        &path,
        Version::parse(env!("CARGO_PKG_VERSION")).expect("package version"),
        &agent::resolved_home()?,
        agent::resolved_codex_home().as_deref(),
        &SkillOverrides::from_env(),
    )
}

fn refresh(
    applied: &Path,
    current: Version,
    home: &Path,
    codex_home: Option<&Path>,
    overrides: &SkillOverrides,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(applied.parent().unwrap())?;
    let lock = std::fs::File::create(applied.with_extension("lock"))?;
    lock.lock()?;
    let previous = match std::fs::read_to_string(applied) {
        Ok(raw) => Some(Version::parse(raw.trim()).context("invalid installed skill version")?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if previous.is_some_and(|version| current <= version) {
        return Ok(());
    }
    let mut visited = std::collections::HashSet::new();
    let mut failures = Vec::new();
    for &skill in Skill::ALL {
        for &target in skill.targets() {
            let paths = std::iter::once(skill.path(target, home, codex_home, overrides))
                .chain(skill.unmanaged_paths(target, home, codex_home, overrides));
            for path in paths {
                if !visited.insert(path.clone()) {
                    continue;
                }
                // Uninstall can leave helper files behind; only an entrypoint opts in.
                match path.symlink_metadata() {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => failures.push(format!("{}: {error}", path.display())),
                    Ok(_) => match skill.install(&path, true) {
                        Ok(outcome) => {
                            tracing::info!(path = %path.display(), ?outcome, "updated installed skill")
                        }
                        Err(error) => failures.push(format!("{}: {error:#}", path.display())),
                    },
                }
            }
        }
    }
    anyhow::ensure!(
        failures.is_empty(),
        "skill updates need attention: {}",
        failures.join("; ")
    );
    std::fs::write(applied, current.to_string()).context("record installed skill version")
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_cli::agent::{DoctorStatus, Target};

    #[test]
    fn new_version_updates_only_existing_entrypoints_and_keeps_backups() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let applied = home.join("applied");
        let overrides = SkillOverrides {
            claude: Some(home.join("custom-claude")),
            ..Default::default()
        };
        let browser = Skill::Browser.path(Target::ClaudeCode, home, None, &overrides);
        let legacy = home.join(".codex/skills/flowmux-browser/SKILL.md");
        let team = Skill::Team.path(Target::Codex, home, None, &overrides);
        let removed = Skill::Team.path(Target::Gemini, home, None, &overrides);
        for path in [&browser, &legacy, &team] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "previous instructions").unwrap();
        }
        Skill::Team.install(&removed, false).unwrap();
        std::fs::remove_file(&removed).unwrap();
        std::fs::write(&applied, "1.0.0").unwrap();
        refresh(&applied, Version(1, 0, 0), home, None, &overrides).unwrap();
        assert_eq!(
            std::fs::read_to_string(&browser).unwrap(),
            "previous instructions"
        );
        assert!(applied.exists());
        refresh(&applied, Version(2, 0, 0), home, None, &overrides).unwrap();
        for (skill, path) in [
            (Skill::Browser, &browser),
            (Skill::Browser, &legacy),
            (Skill::Team, &team),
        ] {
            assert_eq!(skill.doctor(path), DoctorStatus::Ok);
            let backups: Vec<_> = std::fs::read_dir(path.parent().unwrap())
                .unwrap()
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("SKILL.md.flowmux-backup-")
                })
                .collect();
            assert_eq!(backups.len(), 1);
            assert_eq!(
                std::fs::read_to_string(backups[0].path()).unwrap(),
                "previous instructions"
            );
        }
        assert!(!removed.exists());
        assert!(!Skill::Browser
            .path(Target::Cline, home, None, &overrides)
            .exists());
        assert_eq!(std::fs::read_to_string(&applied).unwrap(), "2.0.0");
        std::fs::write(&browser, "edits after update").unwrap();
        refresh(&applied, Version(2, 0, 0), home, None, &overrides).unwrap();
        refresh(&applied, Version(1, 0, 0), home, None, &overrides).unwrap();
        assert_eq!(
            std::fs::read_to_string(&browser).unwrap(),
            "edits after update"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_linked_copy_retries_without_blocking_other_copies() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let applied = home.join("applied");
        let overrides = SkillOverrides::default();
        let linked = Skill::Browser.path(Target::ClaudeCode, home, None, &overrides);
        let regular = Skill::Browser.path(Target::Codex, home, None, &overrides);
        let source = home.join("user-owned");
        std::fs::write(&source, "custom").unwrap();
        std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&source, &linked).unwrap();
        std::fs::create_dir_all(regular.parent().unwrap()).unwrap();
        std::fs::write(&regular, "older").unwrap();
        assert!(refresh(&applied, Version(2, 0, 0), home, None, &overrides).is_err());
        assert_eq!(std::fs::read_to_string(source).unwrap(), "custom");
        assert_eq!(Skill::Browser.doctor(&regular), DoctorStatus::Ok);
        assert!(!applied.exists());
        std::fs::remove_file(linked).unwrap();
        refresh(&applied, Version(2, 0, 0), home, None, &overrides).unwrap();
        assert_eq!(std::fs::read_to_string(&applied).unwrap(), "2.0.0");
    }
}
