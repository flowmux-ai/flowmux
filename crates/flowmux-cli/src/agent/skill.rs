// SPDX-License-Identifier: GPL-3.0-or-later
//! Bundled product skills. Team resources are managed alongside their entrypoint.
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skill {
    Browser,
    Team,
}

impl Skill {
    pub const ALL: &'static [Self] = &[Self::Browser, Self::Team];

    pub fn slug(self) -> &'static str {
        match self {
            Self::Browser => "flowmux-browser",
            Self::Team => "flowmux-team",
        }
    }

    pub fn targets(self) -> &'static [Target] {
        match self {
            Self::Browser => Target::ALL,
            Self::Team => &[Target::ClaudeCode, Target::Codex],
        }
    }

    pub fn is_present(self, path: &Path) -> bool {
        let root = path.parent().expect("skill entrypoint has parent");
        self.files()
            .iter()
            .any(|(name, _)| root.join(name).symlink_metadata().is_ok())
    }

    pub fn codex_unmanaged_paths(self, home: &Path, codex_home: Option<&Path>) -> Vec<PathBuf> {
        let legacy = codex_home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join(".codex"))
            .join("skills")
            .join(self.slug())
            .join("SKILL.md");
        let managed = self.path(Target::Codex, home, codex_home, &SkillOverrides::default());
        if legacy != managed && legacy.exists() {
            vec![legacy]
        } else {
            Vec::new()
        }
    }

    // Install supporting resources first so a new entrypoint is discoverable last.
    pub fn files(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Browser => &[("SKILL.md", SKILL_BODY)],
            Self::Team => &[
                (
                    "scripts/team.py",
                    include_str!("../../../../.agents/skills/flowmux-team/scripts/team.py"),
                ),
                (
                    "references/sample.md",
                    include_str!("../../../../.agents/skills/flowmux-team/references/sample.md"),
                ),
                (
                    "scripts/examples.py",
                    include_str!("../../../../.agents/skills/flowmux-team/scripts/examples.py"),
                ),
                (
                    "references/cookbook.md",
                    include_str!("../../../../.agents/skills/flowmux-team/references/cookbook.md"),
                ),
                (
                    "SKILL.md",
                    include_str!("../../../../.agents/skills/flowmux-team/SKILL.md"),
                ),
            ],
        }
    }

    pub fn path(
        self,
        target: Target,
        home: &Path,
        codex_home: Option<&Path>,
        overrides: &SkillOverrides,
    ) -> PathBuf {
        let browser = overrides.path(target, home, codex_home);
        browser
            .parent()
            .unwrap()
            .with_file_name(self.slug())
            .join("SKILL.md")
    }

    fn check_directories(self, path: &Path) -> Result<()> {
        let root = path.parent().context("skill has no parent")?;
        for (name, _) in self.files() {
            let file = root.join(name);
            for parent in file.ancestors().skip(1) {
                if parent.is_symlink() {
                    anyhow::bail!(
                        "{} is a user-managed linked folder; manage its source manually",
                        parent.display()
                    );
                }
                if parent == root {
                    break;
                }
            }
        }
        Ok(())
    }

    pub fn doctor(self, path: &Path) -> DoctorStatus {
        if self == Self::Browser {
            return doctor_one(path, SKILL_BODY);
        }
        let root = path.parent().expect("skill entrypoint has parent");
        let mut missing = 0;
        let mut drift = false;
        for (name, body) in self.files() {
            match doctor_one(&root.join(name), body) {
                DoctorStatus::Ok => {}
                DoctorStatus::Missing => missing += 1,
                DoctorStatus::Drift => drift = true,
                error @ DoctorStatus::Error(_) => return error,
            }
        }
        if missing == 0 && !drift {
            return DoctorStatus::Ok;
        }
        if let Err(error) = self.check_directories(path) {
            return DoctorStatus::Error(error.to_string());
        }
        if missing == self.files().len() {
            DoctorStatus::Missing
        } else {
            DoctorStatus::Drift
        }
    }

    pub fn install(self, path: &Path, force: bool) -> Result<InstallOutcome> {
        if self == Self::Browser {
            return install_one(path, SKILL_BODY, force);
        }
        if self.doctor(path) == DoctorStatus::Ok {
            return Ok(InstallOutcome::AlreadyUpToDate);
        }
        self.check_directories(path)?;
        let root = path.parent().context("skill has no parent")?;
        // Preflight every resource before changing any file in the bundle.
        for (name, body) in self.files() {
            let file = root.join(name);
            reject_linked_skill(&file)?;
            match doctor_one(&file, body) {
                DoctorStatus::Error(error) => anyhow::bail!("{error}"),
                DoctorStatus::Drift if !force => anyhow::bail!(
                    "{} differs; use Update to back up and replace",
                    file.display()
                ),
                _ => {}
            }
        }
        let mut outcome = InstallOutcome::Written;
        for (name, body) in self.files() {
            if matches!(
                install_one(&root.join(name), body, force)?,
                InstallOutcome::Updated { .. }
            ) {
                outcome = InstallOutcome::Updated {
                    backup: root.to_path_buf(),
                };
            }
        }
        Ok(outcome)
    }

    pub fn removable(self, path: &Path) -> bool {
        if self.check_directories(path).is_err() {
            return false;
        }
        let root = path.parent().expect("skill entrypoint has parent");
        self.files().iter().any(|(name, _)| {
            root.join(name)
                .symlink_metadata()
                .is_ok_and(|meta| meta.is_file() || meta.file_type().is_symlink())
        })
    }

    pub fn uninstall(self, path: &Path) -> Result<UninstallOutcome> {
        if self == Self::Browser {
            return uninstall_one(path);
        }
        self.check_directories(path)?;
        let root = path.parent().context("skill has no parent")?;
        // Refuse unsupported entries before removing the discoverable entrypoint.
        for (name, _) in self.files() {
            let file = root.join(name);
            if !file.is_symlink() {
                match read_skill(&file) {
                    Ok(_) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).with_context(|| format!("reading {}", file.display()))
                    }
                }
            }
        }
        let mut outcome = UninstallOutcome::AlreadyAbsent;
        for (name, body) in self.files().iter().rev() {
            match uninstall_payload(&root.join(name), body)? {
                UninstallOutcome::Preserved { .. } => {
                    outcome = UninstallOutcome::Preserved {
                        backup: root.to_path_buf(),
                    }
                }
                UninstallOutcome::Removed if outcome == UninstallOutcome::AlreadyAbsent => {
                    outcome = UninstallOutcome::Removed
                }
                _ => {}
            }
        }
        // Only empty, known directories are removed; keep backups and user files.
        for directory in [
            root.join("scripts"),
            root.join("references"),
            root.to_path_buf(),
        ] {
            let _ = fs::remove_dir(directory);
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_bundle_repairs_missing_resources_and_preserves_edits() {
        let home = tempfile::tempdir().unwrap();
        let path = Skill::Team.path(Target::Codex, home.path(), None, &SkillOverrides::default());
        let root = path.parent().unwrap();
        assert_eq!(Skill::Team.doctor(&path), DoctorStatus::Missing);
        Skill::Team.install(&path, false).unwrap();
        assert_eq!(Skill::Team.doctor(&path), DoctorStatus::Ok);
        fs::remove_file(root.join("references/sample.md")).unwrap();
        fs::write(root.join("scripts/team.py"), "user edits").unwrap();
        assert_eq!(Skill::Team.doctor(&path), DoctorStatus::Drift);
        assert!(Skill::Team.install(&path, false).is_err());
        assert!(!root.join("references/sample.md").exists());
        assert!(matches!(
            Skill::Team.install(&path, true).unwrap(),
            InstallOutcome::Updated { .. }
        ));
        assert_eq!(Skill::Team.doctor(&path), DoctorStatus::Ok);
        let backups: Vec<_> = fs::read_dir(root.join("scripts"))
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains(".flowmux-backup-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read_to_string(backups[0].path()).unwrap(), "user edits");
        fs::write(root.join("notes.txt"), "keep").unwrap();
        fs::write(root.join("SKILL.md"), "custom instructions").unwrap();
        assert!(matches!(
            Skill::Team.uninstall(&path).unwrap(),
            UninstallOutcome::Preserved { .. }
        ));
        assert_eq!(Skill::Team.doctor(&path), DoctorStatus::Missing);
        assert!(root.join("notes.txt").exists());
        assert!(backups[0].path().exists());
        assert_eq!(
            Skill::Team.uninstall(&path).unwrap(),
            UninstallOutcome::AlreadyAbsent
        );
    }

    #[test]
    fn team_paths_honor_agent_roots_and_clean_uninstall() {
        let home = tempfile::tempdir().unwrap();
        let overrides = SkillOverrides {
            claude: Some(home.path().join("custom")),
            ..Default::default()
        };
        let path = Skill::Team.path(Target::ClaudeCode, home.path(), None, &overrides);
        assert_eq!(
            path,
            home.path().join("custom/skills/flowmux-team/SKILL.md")
        );
        Skill::Team.install(&path, false).unwrap();
        assert_eq!(
            Skill::Team.install(&path, false).unwrap(),
            InstallOutcome::AlreadyUpToDate
        );
        assert_eq!(
            Skill::Team.uninstall(&path).unwrap(),
            UninstallOutcome::Removed
        );
        assert!(!path.parent().unwrap().exists());
    }

    #[cfg(unix)]
    #[test]
    fn linked_team_resources_are_never_modified() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("flowmux-team/SKILL.md");
        Skill::Team.install(&path, false).unwrap();
        let root = path.parent().unwrap();
        let source = home.path().join("original-scripts");
        fs::rename(root.join("scripts"), &source).unwrap();
        symlink(&source, root.join("scripts")).unwrap();
        assert_eq!(Skill::Team.doctor(&path), DoctorStatus::Ok);
        assert!(!Skill::Team.removable(&path));
        fs::write(source.join("team.py"), "custom").unwrap();
        assert!(matches!(Skill::Team.doctor(&path), DoctorStatus::Error(_)));
        assert!(Skill::Team.install(&path, true).is_err());
        assert!(Skill::Team.uninstall(&path).is_err());
        assert!(path.exists());
        assert_eq!(
            fs::read_to_string(source.join("team.py")).unwrap(),
            "custom"
        );
    }
}
