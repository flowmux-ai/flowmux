// SPDX-License-Identifier: GPL-3.0-or-later
//! `flowmux agent install / doctor / uninstall` manages the embedded
//! flowmux-browser skill in each supported agent's user-level skills directory.
//!
//! [`SkillOverrides::path`] resolves the install paths. `doctor` checks
//! the same paths for missing files and content drift against [`SKILL_BODY`].

use anyhow::{anyhow, Context, Result};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// Repository skill embedded at compile time; rebuilding includes updated text.
pub const SKILL_BODY: &str = include_str!("../../../.agents/skills/flowmux-browser/SKILL.md");

/// Resolve environment overrides once at the CLI boundary. Tests can supply
/// explicit roots without changing process-global environment variables.
#[derive(Default)]
pub struct SkillOverrides {
    pub claude: Option<PathBuf>,
    pub opencode: Option<PathBuf>,
}

impl SkillOverrides {
    pub fn from_env() -> Self {
        let nonempty = |name| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        Self {
            claude: nonempty("CLAUDE_CONFIG_DIR"),
            opencode: if flowmux_config::paths::is_flatpak_sandbox() {
                None // The host agent cannot discover sandbox-private XDG paths.
            } else {
                nonempty("XDG_CONFIG_HOME").map(|path| path.join("opencode"))
            },
        }
    }

    pub fn path(&self, target: Target, home: &Path, codex_home: Option<&Path>) -> PathBuf {
        let root = match target {
            Target::ClaudeCode => self.claude.as_ref(),
            Target::OpenCode => self.opencode.as_ref(),
            _ => None,
        };
        match root {
            Some(root) => root.join("skills/flowmux-browser/SKILL.md"),
            None => target.resolved_install_path(home, codex_home),
        }
    }
}

/// Supported agent skill-install targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// `~/.claude/skills/flowmux-browser/SKILL.md`.
    ClaudeCode,
    /// `~/.config/opencode/skills/flowmux-browser/SKILL.md`.
    /// See <https://opencode.ai/docs/skills> for discovery rules.
    OpenCode,
    /// `~/.agents/skills/flowmux-browser/SKILL.md`.
    /// See <https://learn.chatgpt.com/docs/build-skills> for discovery rules.
    Codex,
    /// `~/.gemini/config/skills/flowmux-browser/SKILL.md` (shared Gemini config root).
    Antigravity,
    /// `~/.cline/skills/flowmux-browser/SKILL.md`.
    Cline,
}

impl Target {
    pub const ALL: &'static [Target] = &[
        Target::ClaudeCode,
        Target::OpenCode,
        Target::Codex,
        Target::Antigravity,
        Target::Cline,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Target::ClaudeCode => "claude-code",
            Target::OpenCode => "opencode",
            Target::Codex => "codex",
            Target::Antigravity => "antigravity",
            Target::Cline => "cline",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.slug() == s)
    }

    /// Body that gets written to disk. All supported agents accept the
    /// same `SKILL.md` frontmatter+body shape, so the payload is
    /// uniform.
    pub fn payload() -> &'static str {
        SKILL_BODY
    }

    /// Default install path before config-root overrides. `home` is the resolved
    /// `$HOME` (callers usually pass `dirs::home_dir()`).
    pub fn resolved_install_path(self, home: &Path, _codex_home: Option<&Path>) -> PathBuf {
        match self {
            Target::ClaudeCode => home
                .join(".claude")
                .join("skills")
                .join("flowmux-browser")
                .join("SKILL.md"),
            Target::OpenCode => home
                .join(".config")
                .join("opencode")
                .join("skills")
                .join("flowmux-browser")
                .join("SKILL.md"),
            Target::Codex => home
                .join(".agents")
                .join("skills")
                .join("flowmux-browser")
                .join("SKILL.md"),
            Target::Antigravity => home
                .join(".gemini")
                .join("config")
                .join("skills")
                .join("flowmux-browser")
                .join("SKILL.md"),
            Target::Cline => home
                .join(".cline")
                .join("skills")
                .join("flowmux-browser")
                .join("SKILL.md"),
        }
    }
}

/// Per-target outcome of a `doctor` run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorEntry {
    pub target: Target,
    pub path: PathBuf,
    pub status: DoctorStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorStatus {
    /// File is present and its content matches the embedded payload.
    Ok,
    /// File is present but its content drifted (older flowmux skill or
    /// hand-edited). `flowmux agent install --force` re-syncs it.
    Drift,
    /// File is missing. `flowmux agent install` creates it.
    Missing,
    /// Filesystem error while reading the file (permission etc.).
    Error(String),
}

impl DoctorStatus {
    pub fn label(&self) -> &'static str {
        match self {
            DoctorStatus::Ok => "ok",
            DoctorStatus::Drift => "drift",
            DoctorStatus::Missing => "missing",
            DoctorStatus::Error(_) => "error",
        }
    }
}

/// Idempotent install. Writes `payload` to `path` (creating parent
/// dirs). If `path` already exists with the same content, this is a
/// no-op. If it exists with different content, `force = true`
/// archives the old file then atomically replaces it; `force = false` returns
/// an error. User-managed symlinks are never followed for writes.
pub fn install_one(path: &Path, payload: &str, force: bool) -> Result<InstallOutcome> {
    let existing = match fs::read(path) {
        Ok(existing) => Some(existing),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    if existing.as_deref() == Some(payload.as_bytes()) {
        return Ok(InstallOutcome::AlreadyUpToDate);
    }
    reject_linked_skill(path)?;
    if existing.is_some() && !force {
        return Err(anyhow!(
            "{} exists with different content (run `flowmux agent install --force` to back up and replace)",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating directory {}", parent.display()))?;
    }
    let backup = existing
        .as_deref()
        .map(|bytes| backup_one(path, bytes))
        .transpose()?;
    let mut replacement =
        tempfile::NamedTempFile::new_in(path.parent().context("skill has no parent")?)?;
    replacement.write_all(payload.as_bytes())?;
    if let Ok(metadata) = fs::metadata(path) {
        replacement
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    replacement.as_file().sync_all()?;
    replacement
        .persist(path)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(match backup {
        Some(backup) => InstallOutcome::Updated { backup },
        None => InstallOutcome::Written,
    })
}

fn reject_linked_skill_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if parent.is_symlink() {
            return Err(anyhow!(
                "{} is a user-managed skill directory symlink; manage its source manually",
                parent.display()
            ));
        }
    }
    Ok(())
}

fn reject_linked_skill(path: &Path) -> Result<()> {
    reject_linked_skill_dir(path)?;
    if path.is_symlink() {
        return Err(anyhow!(
            "{} is a user-managed symlink; update its source manually",
            path.display()
        ));
    }
    Ok(())
}

/// A unique, non-SKILL.md filename preserves every revision without being
/// discovered as an active skill. Backups remain after uninstall.
fn backup_one(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let mut backup = tempfile::Builder::new()
        .prefix("SKILL.md.flowmux-backup-")
        .tempfile_in(path.parent().context("skill has no parent")?)?;
    backup.write_all(bytes)?;
    backup.as_file().sync_all()?;
    let (_, backup_path) = backup.keep()?;
    Ok(backup_path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    Written,
    Updated { backup: PathBuf },
    AlreadyUpToDate,
}

/// Compare embedded payload against on-disk file.
pub fn doctor_one(path: &Path, payload: &str) -> DoctorStatus {
    let content = fs::read(path);
    if content
        .as_deref()
        .is_ok_and(|bytes| bytes == payload.as_bytes())
    {
        return DoctorStatus::Ok;
    }
    if let Err(error) = reject_linked_skill(path) {
        return DoctorStatus::Error(error.to_string());
    }
    match content {
        Ok(_) => DoctorStatus::Drift,
        Err(e) if e.kind() == ErrorKind::NotFound => DoctorStatus::Missing,
        Err(e) => DoctorStatus::Error(e.to_string()),
    }
}

/// Resolve the home directory + Codex home for a real run. Tests pass
/// fakes through the lower-level helpers above.
pub fn resolved_home() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow!("HOME is not set; cannot locate user-level dirs"))
}

pub fn resolved_codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME").map(PathBuf::from)
}

pub fn antigravity_is_installed(home: &Path) -> bool {
    home.join(".gemini/antigravity-cli").is_dir() || home.join(".local/bin/agy").is_file()
}

/// Existing legacy skill copies that Codex may discover in addition to the
/// flowmux-managed `~/.agents/skills` copy. These are user-owned, so doctor
/// reports them but install/fix never overwrites or removes them.
pub fn codex_unmanaged_skill_paths(home: &Path, codex_home: Option<&Path>) -> Vec<PathBuf> {
    let managed = Target::Codex.resolved_install_path(home, codex_home);
    let legacy = codex_home
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".codex"))
        .join("skills")
        .join("flowmux-browser")
        .join("SKILL.md");
    if legacy != managed && legacy.exists() {
        vec![legacy]
    } else {
        Vec::new()
    }
}

/// Install for every requested target. Returns one outcome per
/// target; the first error short-circuits.
pub fn install_all(
    targets: &[Target],
    home: &Path,
    codex_home: Option<&Path>,
    force: bool,
    overrides: &SkillOverrides,
) -> Result<Vec<(Target, PathBuf, InstallOutcome)>> {
    let mut out = Vec::with_capacity(targets.len());
    for t in targets {
        let path = overrides.path(*t, home, codex_home);
        let outcome = install_one(&path, Target::payload(), force)?;
        out.push((*t, path, outcome));
    }
    Ok(out)
}

/// Doctor report for every requested target.
pub fn doctor_all(
    targets: &[Target],
    home: &Path,
    codex_home: Option<&Path>,
    overrides: &SkillOverrides,
) -> Vec<DoctorEntry> {
    targets
        .iter()
        .map(|t| {
            let path = overrides.path(*t, home, codex_home);
            let status = doctor_one(&path, Target::payload());
            DoctorEntry {
                target: *t,
                path,
                status,
            }
        })
        .collect()
}

/// Idempotently remove the skill file and its `flowmux-browser` directory
/// when empty. The agent's top-level directory is preserved.
pub fn uninstall_one(path: &Path) -> Result<UninstallOutcome> {
    reject_linked_skill_dir(path)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(UninstallOutcome::AlreadyAbsent)
        }
        Err(error) => return Err(error).with_context(|| format!("inspecting {}", path.display())),
    };
    let backup = if metadata.file_type().is_symlink() {
        // Unlink just this entry, including a dangling link; never its target.
        None
    } else {
        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if bytes == SKILL_BODY.as_bytes() {
            None
        } else {
            Some(backup_one(path, &bytes)?)
        }
    };
    fs::remove_file(path).with_context(|| format!("removing {}", path.display()))?;
    if let Some(parent) = path.parent() {
        if parent.file_name().and_then(|s| s.to_str()) == Some("flowmux-browser") {
            // Empty the dir if we just removed the only file inside.
            if fs::read_dir(parent)
                .map(|mut d| d.next().is_none())
                .unwrap_or(false)
            {
                let _ = fs::remove_dir(parent);
            }
        }
    }
    Ok(match backup {
        Some(backup) => UninstallOutcome::Preserved { backup },
        None => UninstallOutcome::Removed,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstallOutcome {
    Removed,
    Preserved { backup: PathBuf },
    AlreadyAbsent,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_home() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn install_writes_when_missing() {
        let home = fake_home();
        let path = Target::ClaudeCode.resolved_install_path(home.path(), None);
        let outcome = install_one(&path, "hello", false).unwrap();
        assert_eq!(outcome, InstallOutcome::Written);
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn install_is_noop_when_content_matches() {
        let home = fake_home();
        let path = Target::ClaudeCode.resolved_install_path(home.path(), None);
        install_one(&path, "hello", false).unwrap();
        let outcome = install_one(&path, "hello", false).unwrap();
        assert_eq!(outcome, InstallOutcome::AlreadyUpToDate);
    }

    #[test]
    fn install_refuses_overwrite_without_force() {
        let home = fake_home();
        let path = Target::ClaudeCode.resolved_install_path(home.path(), None);
        install_one(&path, "v1", false).unwrap();
        let err = install_one(&path, "v2", false).unwrap_err();
        assert!(err.to_string().contains("--force"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "v1");
    }

    #[test]
    fn install_overwrites_with_force() {
        let home = fake_home();
        let path = Target::ClaudeCode.resolved_install_path(home.path(), None);
        install_one(&path, "v1", false).unwrap();
        let outcome = install_one(&path, "v2", true).unwrap();
        assert!(matches!(outcome, InstallOutcome::Updated { .. }));
        assert_eq!(fs::read_to_string(&path).unwrap(), "v2");
    }

    #[test]
    fn doctor_reports_missing_then_ok_then_drift() {
        let home = fake_home();
        let path = Target::ClaudeCode.resolved_install_path(home.path(), None);

        assert_eq!(doctor_one(&path, "v1"), DoctorStatus::Missing);

        install_one(&path, "v1", false).unwrap();
        assert_eq!(doctor_one(&path, "v1"), DoctorStatus::Ok);

        // Hand-edit on disk → drift.
        fs::write(&path, "edited").unwrap();
        assert_eq!(doctor_one(&path, "v1"), DoctorStatus::Drift);
    }

    #[test]
    fn target_path_layout_per_agent() {
        let home = fake_home();
        let claude = Target::ClaudeCode.resolved_install_path(home.path(), None);
        let opencode = Target::OpenCode.resolved_install_path(home.path(), None);
        let codex = Target::Codex.resolved_install_path(home.path(), None);
        let antigravity = Target::Antigravity.resolved_install_path(home.path(), None);
        let cline = Target::Cline.resolved_install_path(home.path(), None);

        assert!(claude.ends_with(".claude/skills/flowmux-browser/SKILL.md"));
        assert!(opencode.ends_with(".config/opencode/skills/flowmux-browser/SKILL.md"));
        assert!(codex.ends_with(".agents/skills/flowmux-browser/SKILL.md"));
        assert!(antigravity.ends_with(".gemini/config/skills/flowmux-browser/SKILL.md"));
        assert!(cline.ends_with(".cline/skills/flowmux-browser/SKILL.md"));
    }

    #[test]
    fn codex_home_does_not_override_shared_skill_dir() {
        let home = fake_home();
        let codex_home = home.path().join("custom-codex");
        let path = Target::Codex.resolved_install_path(home.path(), Some(&codex_home));
        assert_eq!(
            path,
            home.path()
                .join(".agents")
                .join("skills")
                .join("flowmux-browser")
                .join("SKILL.md"),
        );
    }

    #[test]
    fn codex_unmanaged_skill_paths_reports_legacy_copy() {
        let home = fake_home();
        let legacy = home.path().join(".codex/skills/flowmux-browser/SKILL.md");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, SKILL_BODY).unwrap();

        assert_eq!(codex_unmanaged_skill_paths(home.path(), None), vec![legacy]);
    }

    #[test]
    fn codex_unmanaged_skill_paths_ignores_absent_legacy_copy() {
        let home = fake_home();
        let managed = Target::Codex.resolved_install_path(home.path(), None);
        fs::create_dir_all(managed.parent().unwrap()).unwrap();
        fs::write(&managed, SKILL_BODY).unwrap();

        assert!(codex_unmanaged_skill_paths(home.path(), None).is_empty());
    }

    #[test]
    fn target_from_slug_round_trip() {
        for t in Target::ALL {
            assert_eq!(Target::from_slug(t.slug()), Some(*t));
        }
        assert_eq!(Target::from_slug("nonexistent"), None);
    }

    #[test]
    fn install_all_handles_every_target() {
        let home = fake_home();
        let outcomes = install_all(
            Target::ALL,
            home.path(),
            None,
            false,
            &SkillOverrides::default(),
        )
        .unwrap();
        assert_eq!(outcomes.len(), Target::ALL.len());
        for (_t, path, outcome) in &outcomes {
            assert_eq!(*outcome, InstallOutcome::Written);
            assert!(path.exists());
        }
    }

    #[test]
    fn doctor_all_reports_one_entry_per_target_after_partial_install() {
        let home = fake_home();
        // Install Claude only.
        install_one(
            &Target::ClaudeCode.resolved_install_path(home.path(), None),
            Target::payload(),
            false,
        )
        .unwrap();

        let report = doctor_all(Target::ALL, home.path(), None, &SkillOverrides::default());
        assert_eq!(report.len(), Target::ALL.len());

        let by_target: std::collections::HashMap<_, _> =
            report.iter().map(|e| (e.target, &e.status)).collect();
        assert_eq!(by_target[&Target::ClaudeCode], &DoctorStatus::Ok);
        assert_eq!(by_target[&Target::OpenCode], &DoctorStatus::Missing);
        assert_eq!(by_target[&Target::Codex], &DoctorStatus::Missing);
        assert_eq!(by_target[&Target::Antigravity], &DoctorStatus::Missing);
        assert_eq!(by_target[&Target::Cline], &DoctorStatus::Missing);
    }

    #[test]
    fn uninstall_removes_then_reports_absent() {
        let home = fake_home();
        let path = Target::ClaudeCode.resolved_install_path(home.path(), None);
        install_one(&path, SKILL_BODY, false).unwrap();
        assert_eq!(uninstall_one(&path).unwrap(), UninstallOutcome::Removed);
        assert!(!path.exists());
        // The empty `flowmux-browser/` parent should be cleaned up too.
        assert!(!path.parent().unwrap().exists());

        // Second uninstall is idempotent.
        assert_eq!(
            uninstall_one(&path).unwrap(),
            UninstallOutcome::AlreadyAbsent
        );
    }

    #[test]
    fn embedded_payload_is_not_empty() {
        // Sanity: include_str! resolved to the real SKILL body.
        assert!(SKILL_BODY.contains("flowmux"));
        assert!(SKILL_BODY.contains("snapshot"));
    }

    #[test]
    fn supported_targets_share_the_same_skill_payload() {
        assert_eq!(Target::payload(), SKILL_BODY);
    }

    #[test]
    fn every_target_writes_into_a_skills_directory() {
        let home = fake_home();
        for t in Target::ALL {
            let p = t.resolved_install_path(home.path(), None);
            assert_eq!(
                p.file_name().and_then(|s| s.to_str()),
                Some("SKILL.md"),
                "{:?} should end in SKILL.md, got {}",
                t,
                p.display(),
            );
            assert_eq!(
                p.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str()),
                Some("flowmux-browser"),
                "{:?} skill dir should be flowmux-browser, got {}",
                t,
                p.display(),
            );
            assert_eq!(
                p.parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str()),
                Some("skills"),
                "{:?} should live under a skills/ dir, got {}",
                t,
                p.display(),
            );
        }
    }

    /// Scenario: full install → doctor reports OK → simulate a flowmux
    /// upgrade that ships an updated SKILL → doctor reports Drift on
    /// every target → `--force` install brings them all back to OK.
    #[test]
    fn scenario_upgrade_drift_then_reinstall_with_force() {
        let home = fake_home();

        // 1. Initial install with embedded SKILL.
        install_all(
            Target::ALL,
            home.path(),
            None,
            false,
            &SkillOverrides::default(),
        )
        .unwrap();
        let report = doctor_all(Target::ALL, home.path(), None, &SkillOverrides::default());
        assert!(report.iter().all(|e| e.status == DoctorStatus::Ok));

        // 2. Pretend the embedded SKILL changed by writing an older
        //    body to every install path.
        for t in Target::ALL {
            let p = t.resolved_install_path(home.path(), None);
            fs::write(p, "old payload").unwrap();
        }
        let report = doctor_all(Target::ALL, home.path(), None, &SkillOverrides::default());
        assert!(report.iter().all(|e| e.status == DoctorStatus::Drift));

        // 3. Re-install with --force restores parity.
        install_all(
            Target::ALL,
            home.path(),
            None,
            true,
            &SkillOverrides::default(),
        )
        .unwrap();
        let report = doctor_all(Target::ALL, home.path(), None, &SkillOverrides::default());
        assert!(report.iter().all(|e| e.status == DoctorStatus::Ok));
    }

    #[test]
    fn forced_update_preserves_previous_bytes() {
        let home = fake_home();
        let path = Target::Codex.resolved_install_path(home.path(), None);
        install_one(&path, "user instructions", false).unwrap();
        install_one(&path, SKILL_BODY, true).unwrap();
        let backups: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|entry| entry != &path)
            .collect();
        assert_eq!(backups.len(), 1, "an update must retain the previous file");
        assert_eq!(
            fs::read_to_string(&backups[0]).unwrap(),
            "user instructions"
        );
        install_one(&path, SKILL_BODY, true).unwrap();
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 2);
    }

    #[test]
    fn uninstall_preserves_modified_skill_and_supporting_files() {
        let home = fake_home();
        let path = Target::Codex.resolved_install_path(home.path(), None);
        install_one(&path, "user instructions", false).unwrap();
        let supporting = path.with_file_name("notes.txt");
        fs::write(&supporting, "notes").unwrap();
        uninstall_one(&path).unwrap();
        assert!(!path.exists());
        let files: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect();
        assert!(files.contains(&"user instructions".to_string()));
        assert_eq!(fs::read_to_string(supporting).unwrap(), "notes");
    }

    #[cfg(unix)]
    #[test]
    fn linked_skill_is_not_overwritten_even_with_force() {
        let home = fake_home();
        let path = Target::Codex.resolved_install_path(home.path(), None);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = home.path().join("original.md");
        fs::write(&original, "user instructions").unwrap();
        std::os::unix::fs::symlink(&original, &path).unwrap();
        assert!(install_one(&path, SKILL_BODY, true).is_err());
        assert_eq!(fs::read_to_string(original).unwrap(), "user instructions");
        assert!(path.is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn dangling_link_is_diagnosed_and_can_be_uninstalled() {
        let home = fake_home();
        let path = Target::Codex.resolved_install_path(home.path(), None);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("missing.md", &path).unwrap();
        assert!(matches!(
            doctor_one(&path, SKILL_BODY),
            DoctorStatus::Error(_)
        ));
        assert!(install_one(&path, SKILL_BODY, true).is_err());
        assert_eq!(uninstall_one(&path).unwrap(), UninstallOutcome::Removed);
        assert!(!path.is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn linked_skill_directory_preserves_its_source() {
        let home = fake_home();
        let path = Target::Codex.resolved_install_path(home.path(), None);
        let source = home.path().join("dotfiles");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "user instructions").unwrap();
        fs::create_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&source, path.parent().unwrap()).unwrap();
        assert!(install_one(&path, SKILL_BODY, true).is_err());
        assert!(uninstall_one(&path).is_err());
        assert_eq!(
            fs::read_to_string(source.join("SKILL.md")).unwrap(),
            "user instructions"
        );
    }

    #[cfg(unix)]
    #[test]
    fn current_linked_skills_are_healthy_without_replacing_links() {
        for link_directory in [false, true] {
            let home = fake_home();
            let path = Target::Codex.resolved_install_path(home.path(), None);
            let source = home.path().join("dotfiles");
            fs::create_dir_all(&source).unwrap();
            fs::write(source.join("SKILL.md"), SKILL_BODY).unwrap();
            let link = if link_directory {
                path.parent().unwrap()
            } else {
                &path
            };
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            let target = if link_directory {
                source.clone()
            } else {
                source.join("SKILL.md")
            };
            std::os::unix::fs::symlink(target, link).unwrap();
            assert_eq!(doctor_one(&path, SKILL_BODY), DoctorStatus::Ok);
            assert_eq!(
                install_one(&path, SKILL_BODY, true).unwrap(),
                InstallOutcome::AlreadyUpToDate
            );
            assert!(link.is_symlink());
        }
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replacement_preserves_mode_and_does_not_truncate_hardlinks() {
        use std::os::unix::fs::PermissionsExt;
        let home = fake_home();
        let path = Target::Codex.resolved_install_path(home.path(), None);
        install_one(&path, "user instructions", false).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let other = home.path().join("other.md");
        fs::hard_link(&path, &other).unwrap();
        install_one(&path, SKILL_BODY, true).unwrap();
        assert_eq!(fs::read_to_string(other).unwrap(), "user instructions");
        assert_eq!(fs::read_to_string(&path).unwrap(), SKILL_BODY);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}
