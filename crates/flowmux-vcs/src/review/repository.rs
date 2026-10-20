// SPDX-License-Identifier: GPL-3.0-or-later
//! A checkout and the connection used to read it. Remote paths never touch local I/O.
use super::*;
use flowmux_core::SshTarget;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub root: PathBuf,
    ssh: Option<(SshTarget, PathBuf)>,
}

impl Repository {
    pub fn local(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ssh: None,
        }
    }

    pub fn ssh(target: SshTarget, socket: PathBuf, cwd: Option<&str>) -> Result<Self, String> {
        flowmux_core::ssh::validate_remote_cwd(cwd)?;
        target.command_argv(&socket, OsStr::new("true"))?;
        Ok(Self {
            root: cwd.unwrap_or(".").into(),
            ssh: Some((target, socket)),
        })
    }

    pub fn discover(&self) -> Result<Self, String> {
        let bytes = git(self, &args(&["rev-parse", "--show-toplevel"]), false)?;
        let mut repository = self.clone();
        let root = PathBuf::from(OsString::from_vec(
            bytes.strip_suffix(b"\n").unwrap_or(&bytes).to_vec(),
        ));
        repository.root = if self.ssh.is_some() {
            root
        } else {
            std::fs::canonicalize(&root).unwrap_or(root)
        };
        Ok(repository)
    }

    pub fn display(&self) -> String {
        match &self.ssh {
            Some((target, _)) => format!("{}:{}", target.destination(), display_path(&self.root)),
            None => display_path(&self.root),
        }
    }

    /// Preserve existing local draft keys; exclude the transient SSH socket.
    pub fn storage_key(&self) -> Vec<u8> {
        let mut key = Vec::new();
        if let Some((target, _)) = &self.ssh {
            key.extend_from_slice(b"ssh\0");
            key.extend(serde_json::to_vec(target).expect("SSH target is serializable"));
            key.push(0);
        }
        key.extend_from_slice(self.root.as_os_str().as_bytes());
        key
    }

    pub fn load(&self, scope: Scope) -> Result<Snapshot, String> {
        load_repository(self, scope)
    }

    pub fn history(&self, tip: Option<&str>, offset: usize) -> Result<history::Page, String> {
        history::load_repository(self, tip, offset)
    }

    pub(super) fn command(&self, command: &[OsString]) -> Result<Command, String> {
        if self.ssh.is_some() {
            let mut script = b"exec env GIT_OPTIONAL_LOCKS=0 ".to_vec();
            script.extend(shell_words(command));
            self.remote_command(&script)
        } else {
            let mut process = Command::new(&command[0]);
            process
                .args(&command[1..])
                .current_dir(&self.root)
                .env("GIT_OPTIONAL_LOCKS", "0");
            Ok(process)
        }
    }

    fn remote_command(&self, script: &[u8]) -> Result<Command, String> {
        let (target, socket) = self.ssh.as_ref().ok_or("Missing SSH connection")?;
        let mut bootstrap = b"cd ".to_vec();
        bootstrap.extend(quote(self.root.as_os_str()));
        // Exit 1 is reserved for a successful `git diff --no-index` with changes.
        bootstrap.extend_from_slice(b" || exit 2; ");
        bootstrap.extend_from_slice(script);
        let argv = target.command_argv(socket, &OsString::from_vec(bootstrap))?;
        let mut process = Command::new(&argv[0]);
        process.args(&argv[1..]);
        Ok(process)
    }

    pub(super) fn untracked_patch(&self, file: &File) -> Result<Vec<u8>, String> {
        let mut command = git_args();
        command.extend(args(&[
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--full-index",
            "--",
            "/dev/null",
        ]));
        command.push(file.path.as_os_str().into());
        if self.ssh.is_some() {
            // Check remotely before Git reads: FIFOs must not hang and symlinks
            // must not expose their targets. Prefix relative filenames for tools.
            let path = Path::new(".").join(&file.path);
            let mut script = b"p=".to_vec();
            script.extend(quote(path.as_os_str()));
            // A NUL distinguishes raw link targets from Git's textual diff.
            script.extend_from_slice(b"; if [ -L \"$p\" ]; then printf '\\0'");
            script.extend_from_slice(
                br#"; readlink "$p" || exit 2
elif [ ! -f "$p" ]; then
    echo 'This untracked path is not a regular file.' >&2; exit 2
else
    size=$(wc -c < "$p") || exit 2
    if [ "$size" -gt 8388608 ]; then
        echo 'This untracked file exceeds the 8 MiB display limit.' >&2; exit 2
    fi
    exec env GIT_OPTIONAL_LOCKS=0 "#,
            );
            script.extend(shell_words(&command));
            script.extend_from_slice(b"; fi");
            let bytes = run(&mut self.remote_command(&script)?, true)?;
            return Ok(match bytes.strip_prefix(b"\0") {
                Some(target) => symlink_patch(
                    file,
                    Path::new(OsStr::from_bytes(
                        target.strip_suffix(b"\n").unwrap_or(target),
                    )),
                ),
                None => bytes,
            });
        }
        let path = self.root.join(&file.path);
        let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Ok(symlink_patch(
                file,
                &std::fs::read_link(path).map_err(|e| e.to_string())?,
            ));
        }
        if !meta.is_file() {
            return Err("This untracked path is not a regular file.".into());
        }
        if meta.len() > OUTPUT_LIMIT as u64 {
            return Err("This untracked file exceeds the 8 MiB display limit.".into());
        }
        run(&mut self.command(&command)?, true)
    }
}

fn symlink_patch(file: &File, target: &Path) -> Vec<u8> {
    format!(
        "New symbolic link: {} → {}\n",
        file.label(),
        display_path(target)
    )
    .into_bytes()
}

fn quote(value: &OsStr) -> Vec<u8> {
    let mut quoted = vec![b'\''];
    for byte in value.as_bytes() {
        if *byte == b'\'' {
            quoted.extend_from_slice(b"'\\''");
        } else {
            quoted.push(*byte);
        }
    }
    quoted.push(b'\'');
    quoted
}

fn shell_words(words: &[OsString]) -> Vec<u8> {
    words
        .iter()
        .map(|word| quote(word))
        .collect::<Vec<_>>()
        .join(&b' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_ids_reject_remote_shell_noise_before_display_or_reuse() {
        for length in [40, 64] {
            let oid = "a".repeat(length);
            assert_eq!(parse_oid(format!("{oid}\n").as_bytes()).unwrap(), oid);
        }
        for value in [
            "",
            "short",
            "--all",
            "한글",
            "welcome\n0123456789",
            &"x".repeat(40),
        ] {
            assert!(parse_oid(value.as_bytes()).is_err(), "{value}");
        }
    }

    #[test]
    fn remote_shell_preserves_paths_bytes_and_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("한글 ' $(touch injected) `touch injected`");
        std::fs::create_dir(&path).unwrap();
        let repo = Repository::ssh(
            SshTarget::parse("devbox").unwrap(),
            dir.path().join("mux"),
            path.to_str(),
        )
        .unwrap();
        let values = [
            OsString::from(""),
            OsString::from("'\n$(touch injected)"),
            OsString::from_vec(vec![b'x', 0xff]),
        ];
        let mut command = args(&["printf", "%s\\0"]);
        command.extend(values.clone());
        let process = repo.command(&command).unwrap();
        let argv = process.get_args().collect::<Vec<_>>();
        assert!(argv.contains(&OsStr::new("-T")));
        assert!(argv.contains(&OsStr::new("ProxyCommand=/bin/false")));
        assert!(argv.contains(&OsStr::new("BatchMode=yes")));
        let output = Command::new("sh")
            .arg("-c")
            .arg(argv.last().unwrap())
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        let expected = values
            .iter()
            .flat_map(|v| v.as_bytes().iter().copied().chain([0]))
            .collect::<Vec<_>>();
        assert_eq!(output.stdout, expected);
        assert!(!path.join("injected").exists());
        assert!(!dir.path().join("injected").exists());
        assert!(repo.command(&[OsString::from_vec(vec![b'x', 0])]).is_err());
        std::fs::remove_dir(&path).unwrap();
        let output = Command::new("sh")
            .arg("-c")
            .arg(argv.last().unwrap())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }

    #[test]
    fn missing_master_never_reads_the_local_checkout() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .arg(dir.path())
            .status()
            .unwrap()
            .success());
        assert!(Repository::local(dir.path())
            .load(Scope::WorkingTree)
            .is_ok());
        let remote = Repository::ssh(
            SshTarget::parse("127.0.0.1").unwrap(),
            dir.path().join("missing-master"),
            dir.path().to_str(),
        )
        .unwrap();
        assert!(remote.load(Scope::WorkingTree).is_err());
        assert!(remote.history(None, 0).is_err());
    }
}
