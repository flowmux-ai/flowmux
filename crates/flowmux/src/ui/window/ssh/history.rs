// SPDX-License-Identifier: GPL-3.0-or-later
//! Dialog defaults live entirely in the OS credential store, never in a sidecar.
use flowmux_core::SshWorkspaceConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// Serialize reads and writes before entering the blocking pool so an older
// connection's delayed save cannot replace a newer successful connection.
static ACCESS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const UNAVAILABLE: &str =
    "SSH history unavailable. Unlock or enable your system keyring and try again.";
const INVALID: &str = "Saved SSH history is invalid; enter the connection settings again.";
const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Connection {
    pub name: Option<String>,
    pub config: SshWorkspaceConfig,
}

fn entry() -> Result<keyring::Entry, &'static str> {
    let path = flowmux_config::paths::state_dir().ok_or(UNAVAILABLE)?;
    // Keep separate profiles isolated without exposing paths in keyring metadata.
    let profile = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
    keyring::Entry::new("com.flowmux.App.ssh-history.v1", &profile).map_err(|_| UNAVAILABLE)
}

pub(super) async fn load() -> Result<Option<Connection>, &'static str> {
    let _guard = ACCESS.lock().await;
    gtk::gio::spawn_blocking(|| read(&entry()?))
        .await
        .map_err(|_| UNAVAILABLE)?
}

pub(super) async fn save(connection: Connection) -> Result<(), &'static str> {
    let _guard = ACCESS.lock().await;
    gtk::gio::spawn_blocking(move || write(&entry()?, connection))
        .await
        .map_err(|_| UNAVAILABLE)?
}

fn validate(connection: &Connection) -> Result<(), &'static str> {
    connection.config.validate().map_err(|_| INVALID)?;
    if !connection.config.forwards.is_empty()
        || connection
            .name
            .as_ref()
            .is_some_and(|name| name.contains('\0'))
    {
        return Err(INVALID);
    }
    Ok(())
}

fn read(entry: &keyring::Entry) -> Result<Option<Connection>, &'static str> {
    let bytes = match entry.get_secret() {
        Ok(bytes) => bytes,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(_) => return Err(UNAVAILABLE),
    };
    if bytes.len() > MAX_BYTES {
        return Err(INVALID);
    }
    let connection = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    validate(&connection)?;
    Ok(Some(connection))
}

fn write(entry: &keyring::Entry, mut connection: Connection) -> Result<(), &'static str> {
    // Port forwards and one-shot commands are not New SSH Workspace inputs.
    connection.config.forwards.clear();
    validate(&connection)?;
    let bytes = serde_json::to_vec(&connection).map_err(|_| INVALID)?;
    if bytes.len() > MAX_BYTES {
        return Err(INVALID);
    }
    entry.set_secret(&bytes).map_err(|_| UNAVAILABLE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_core::{SshForwardSpec, SshTarget};
    use keyring::mock::MockCredential;

    fn fixture() -> (keyring::Entry, Connection) {
        let entry = keyring::Entry::new_with_credential(Box::new(MockCredential::default()));
        let connection = Connection {
            name: Some("개발 서버".into()),
            config: SshWorkspaceConfig {
                target: SshTarget {
                    port: Some(2222),
                    identity_file: Some("/keys/my key".into()),
                    config_file: Some("/ssh/config".into()),
                    ..SshTarget::parse("alice@[::1]").unwrap()
                },
                cwd: Some("/srv/my project".into()),
                tmux: true,
                forwards: vec![],
            },
        };
        (entry, connection)
    }

    #[test]
    fn history_round_trip_excludes_forwarding() {
        let (entry, connection) = fixture();
        assert!(read(&entry).unwrap().is_none());
        let mut saved = connection.clone();
        saved.config.forwards.push(SshForwardSpec {
            id: uuid::Uuid::new_v4(),
            remote_port: 3000,
            local_port: None,
            https: false,
        });
        write(&entry, saved).unwrap();
        assert!(read(&entry).unwrap() == Some(connection));
    }

    #[test]
    fn history_rejects_invalid_data_without_exposing_or_replacing_it() {
        let (entry, connection) = fixture();
        for bytes in [b"private broken JSON".to_vec(), vec![0; MAX_BYTES + 1]] {
            entry.set_secret(&bytes).unwrap();
            assert!(matches!(read(&entry), Err(INVALID)));
            assert_eq!(entry.get_secret().unwrap(), bytes);
        }
        let mut invalid = connection.clone();
        invalid.config.target.host = "-private-invalid-host".into();
        entry
            .set_secret(&serde_json::to_vec(&invalid).unwrap())
            .unwrap();
        assert!(matches!(read(&entry), Err(INVALID)));
        write(&entry, connection.clone()).unwrap();
        assert_eq!(write(&entry, invalid), Err(INVALID));
        assert!(read(&entry).unwrap() == Some(connection));
    }

    #[test]
    fn history_store_failure_preserves_previous_success() {
        let (entry, connection) = fixture();
        write(&entry, connection.clone()).unwrap();
        let mock = entry
            .get_credential()
            .downcast_ref::<MockCredential>()
            .unwrap();
        mock.set_error(keyring::Error::NoStorageAccess(Box::new(
            std::io::Error::other("private error details"),
        )));
        let mut next = connection.clone();
        next.config.target.host = "next-host".into();
        assert_eq!(write(&entry, next), Err(UNAVAILABLE));
        assert!(read(&entry).unwrap() == Some(connection));
        mock.set_error(keyring::Error::NoStorageAccess(Box::new(
            std::io::Error::other("private error details"),
        )));
        assert!(matches!(read(&entry), Err(UNAVAILABLE)));
    }
}
