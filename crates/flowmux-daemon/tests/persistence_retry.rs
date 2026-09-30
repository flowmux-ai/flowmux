// SPDX-License-Identifier: GPL-3.0-or-later

use std::{process::Command, time::Duration};

#[test]
fn persistence_recovers_without_another_mutation() {
    // Keep XDG overrides in a subprocess so other tests and user state are isolated.
    if std::env::var_os("FLOWMUX_PERSISTENCE_RETRY_TEST").is_none() {
        let dir = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "persistence_recovers_without_another_mutation",
                "--nocapture",
            ])
            .env("FLOWMUX_PERSISTENCE_RETRY_TEST", "1")
            .env("XDG_STATE_HOME", dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let path = flowmux_state::default_path().unwrap();
            std::fs::create_dir_all(&path).unwrap(); // A directory cannot be read as state.json.
            let log_path = path.with_extension("log");
            let log = std::fs::File::create(&log_path).unwrap();
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(move || log.try_clone().unwrap())
                .finish();
            let _guard = tracing::subscriber::set_default(subscriber);
            let store =
                flowmux_daemon::state_store::StateStore::new_lazy(flowmux_state::State::default());
            let workspace = store
                .create_workspace(Some("retry proof".into()), std::env::temp_dir())
                .await;
            let writer = {
                let store = store.clone();
                tokio::spawn(async move { store.persist_loop().await })
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                while !std::fs::read_to_string(&log_path)
                    .unwrap()
                    .contains("state save failed")
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("first save must fail before recovery");
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert_eq!(
                std::fs::read_to_string(&log_path)
                    .unwrap()
                    .matches("state save failed")
                    .count(),
                1,
                "failed saves must not spin"
            );
            std::fs::remove_dir(&path).unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !path.is_file() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("save must retry without another mutation");
            assert!(flowmux_state::load()
                .unwrap()
                .workspaces
                .iter()
                .any(|ws| ws.id == workspace));
            let saved = std::fs::read(&path).unwrap();
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert_eq!(
                std::fs::read(&path).unwrap(),
                saved,
                "successful generations must not be written again"
            );
            writer.abort();
        });
}
