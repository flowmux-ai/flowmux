// SPDX-License-Identifier: GPL-3.0-or-later
//! Bundle updates are deliberately separate from the Linux tarball transaction.
use super::*;

fn bundle_path(executable: &Path) -> Option<&Path> {
    let bundle = executable.parent()?.parent()?.parent()?;
    (executable.strip_prefix(bundle).ok()? == Path::new("Contents/MacOS/flowmux")
        && bundle
            .file_name()?
            .to_str()?
            .eq_ignore_ascii_case("Flowmux.app"))
    .then_some(bundle)
}

pub(super) async fn install(
    version: Version,
    tx: &async_channel::Sender<Event>,
    log: &mut File,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        std::env::consts::ARCH == "aarch64",
        "macOS release DMGs currently require Apple Silicon"
    );
    let executable = std::env::current_exe()?.canonicalize()?;
    let bundle =
        bundle_path(&executable).context("running executable is not inside Flowmux.app")?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(
            bundle
                .parent()
                .context("app has no parent")?
                .join(".Flowmux.app.update-lock"),
        )
        .context("app directory is not writable; update this app manually")?;
    lock.try_lock().context("another app update is running")?;
    let cache = flowmux_config::paths::host_visible_cache_dir().context("HOME is unset")?;
    std::fs::create_dir_all(&cache)?;
    let work = cache.join(format!(".macos-update-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&work)?;
    let result = async {
        let asset = format!("flowmux-{version}-aarch64-apple-darwin.dmg");
        let url = format!("{RELEASE_DOWNLOAD_URL}/{}/{asset}", version.tag());
        let archive = work.join(&asset);
        let fetching = Progress::new(Stage::Fetching, tx);
        let _ = tx.send(Event::Progress(Stage::Fetching, 0)).await;
        download(&url, &archive, &fetching).await?;
        writeln!(log, "release_asset={asset} checksum=verified")?;
        let script = work.join("update.sh");
        std::fs::write(
            &script,
            include_str!("../../../../scripts/update-macos-dmg.sh"),
        )?;
        let _ = tx.send(Event::Progress(Stage::Installing, 0)).await;
        let installing = Progress::new(Stage::Installing, tx);
        let mut argv = vec!["/bin/sh".into()];
        for path in [&script, &archive, bundle] {
            argv.push(
                path.to_str()
                    .context("update path must be UTF-8")?
                    .to_owned(),
            );
        }
        argv.push(version.to_string());
        run_logged(argv, None, None, log, installing.clone()).await?;
        installing.report(100);
        Ok(())
    }
    .await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

async fn download(url: &str, path: &Path, progress: &Progress) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()?;
    let checksum = client
        .get(format!("{url}.sha256"))
        .send()
        .await?
        .error_for_status()
        .context("release has no downloadable DMG checksum")?
        .text()
        .await?;
    let expected = expected_sha256(&checksum).context("invalid DMG checksum")?;
    let mut response = client
        .get(url)
        .send()
        .await?
        .error_for_status()
        .context("release has no downloadable DMG")?;
    let total = response.content_length().filter(|total| *total > 0);
    let mut file = tokio::fs::File::create(path).await?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    while let Some(chunk) = response.chunk().await? {
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        if let Some(total) = total {
            progress.report((downloaded.saturating_mul(99) / total).min(99) as u8);
        }
    }
    file.flush().await?;
    anyhow::ensure!(
        format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(expected),
        "DMG checksum mismatch; existing installation unchanged"
    );
    progress.report(100);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_is_the_running_bundle_not_a_default_install_directory() {
        for root in [
            "/Applications/Flowmux.app",
            "/Users/test/Apps with spaces/FLOWMUX.APP",
        ] {
            let executable = Path::new(root).join("Contents/MacOS/flowmux");
            assert_eq!(bundle_path(&executable), Some(Path::new(root)));
        }
        for path in [
            "/tmp/flowmux",
            "/tmp/Flowmux.app/other/flowmux",
            "/tmp/Other.app/Contents/MacOS/flowmux",
        ] {
            assert!(bundle_path(Path::new(path)).is_none());
        }
    }

    #[tokio::test]
    async fn download_requires_matching_checksum_and_an_available_asset() {
        use std::net::TcpListener;
        for (checksum, status, expected) in [
            (format!("{:x}", Sha256::digest(b"fixture")), "200 OK", true),
            ("0".repeat(64), "200 OK", false),
            ("bad".into(), "200 OK", false),
            ("missing".into(), "404 Not Found", false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/fixture.dmg", listener.local_addr().unwrap());
            let requests = if checksum.len() == 64 { 2 } else { 1 };
            let server = std::thread::spawn(move || {
                for _ in 0..requests {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut request = [0; 4096];
                    let n = stream.read(&mut request).unwrap();
                    let text = String::from_utf8_lossy(&request[..n]);
                    let body = if text.contains(".sha256 ") {
                        &checksum
                    } else {
                        "fixture"
                    };
                    write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            });
            let temp = tempfile::tempdir().unwrap();
            let (tx, _rx) = async_channel::unbounded();
            let result = download(
                &url,
                &temp.path().join("fixture.dmg"),
                &Progress::new(Stage::Fetching, &tx),
            )
            .await;
            assert_eq!(result.is_ok(), expected, "{result:?}");
            server.join().unwrap();
        }
    }
}
