// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded viewport recording through the existing, surface-pinned capture API.
// shortcut: native snapshots can omit accelerated layers; use compositor capture for video/WebGL coverage.
use super::*;
use std::{io::Write, process::Stdio, time::Duration};
use tokio::time::{timeout, Instant};

pub(crate) async fn diagnose() -> serde_json::Value {
    let encoder = timeout(
        Duration::from_secs(3),
        tokio::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let (supported, reason) = match encoder {
        Ok(Ok(output)) if output.status.success() => {
            let available = String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some("libx264"));
            (available, (!available).then_some("libx264_missing"))
        }
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            (false, Some("ffmpeg_missing"))
        }
        Err(_) => (false, Some("encoder_probe_timeout")),
        _ => (false, Some("encoder_probe_failed")),
    };
    serde_json::json!({
        "encoder_available": supported, "reason": reason, "probe_location": "cli_path",
        "next_action": if supported { None } else { Some("Provide ffmpeg with libx264 on the CLI's PATH") },
        "audio": false, "max_seconds": 60, "max_fps": 10,
        "may_omit_accelerated_layers": true,
    })
}

pub(crate) async fn record(
    client: &Client,
    pane: PaneId,
    surface: Option<SurfaceId>,
    path: PathBuf,
    seconds: u64,
    fps: u32,
) -> anyhow::Result<serde_json::Value> {
    let path = std::path::absolute(path)?;
    anyhow::ensure!(
        path.extension().is_some_and(|ext| ext == "mp4"),
        "recording path must end in .mp4"
    );
    anyhow::ensure!(!path.try_exists()?, "recording destination already exists");
    let Response::Tree { workspaces } =
        timeout(Duration::from_secs(10), client.call(Request::WorkspaceTree)).await??
    else {
        anyhow::bail!("could not resolve the recording tab");
    };
    let tab = workspaces
        .iter()
        .flat_map(|workspace| &workspace.panes)
        .find(|candidate| candidate.id == pane)
        .and_then(|pane| {
            pane.tabs
                .iter()
                .find(|tab| surface.map_or(tab.active, |id| tab.id == id))
        })
        .context("recording tab does not exist")?;
    anyhow::ensure!(tab.kind == "browser", "recording requires a browser tab");
    let surface = tab.id;
    let frames = tempfile::tempdir()?;
    let output =
        tempfile::NamedTempFile::new_in(path.parent().context("recording path has no parent")?)?;
    let started = Instant::now();
    let duration = Duration::from_secs(seconds);
    let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
    let mut captured = Vec::new();
    let mut dimensions = None;
    let mut bytes = 0;
    while started.elapsed() < duration {
        let name = format!("{:04}.png", captured.len());
        let frame = frames.path().join(&name);
        let response = timeout(
            Duration::from_secs(10),
            client.call(Request::BrowserOnSurface {
                surface,
                request: Box::new(Request::BrowserScreenshot {
                    pane,
                    path: frame.clone(),
                    full_page: false,
                    element: None,
                }),
            }),
        )
        .await??;
        anyhow::ensure!(
            matches!(response, Response::BrowserResult { .. }),
            "browser capture failed: {response:?}"
        );
        let data = std::fs::read(&frame)?;
        anyhow::ensure!(
            data.starts_with(b"\x89PNG\r\n\x1a\n") && data.len() >= 24,
            "browser capture is not a PNG"
        );
        let width = u32::from_be_bytes(data[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(data[20..24].try_into().unwrap());
        anyhow::ensure!(
            width > 0 && height > 0 && u64::from(width) * u64::from(height) <= 16 * 1024 * 1024,
            "browser recording frame exceeds the 64 MiB pixel budget"
        );
        dimensions.get_or_insert((width + width % 2, height + height % 2));
        bytes += data.len();
        anyhow::ensure!(
            bytes <= 256 * 1024 * 1024,
            "browser recording exceeds the 256 MiB frame budget"
        );
        captured.push((name, started.elapsed()));
        let next = started
            + interval
                .mul_f64((started.elapsed().as_secs_f64() / interval.as_secs_f64()).floor() + 1.0);
        tokio::time::sleep_until(next.min(started + duration)).await;
    }
    anyhow::ensure!(!captured.is_empty(), "browser recording contains no frames");
    // Actual capture times preserve playback speed when snapshots miss a sampling deadline.
    captured[0].1 = Duration::ZERO;
    let elapsed = started.elapsed();
    let mut manifest = std::fs::File::create(frames.path().join("frames.txt"))?;
    writeln!(manifest, "ffconcat version 1.0")?;
    for (index, (name, at)) in captured.iter().enumerate() {
        let end = captured.get(index + 1).map_or(elapsed, |frame| frame.1);
        writeln!(
            manifest,
            "file '{name}'\nduration {:.6}",
            (end - *at).as_secs_f64()
        )?;
    }
    writeln!(manifest, "file '{}'", captured.last().unwrap().0)?;
    let (width, height) = dimensions.unwrap();
    let fit = format!("scale={width}:{height}:force_original_aspect_ratio=decrease:force_divisible_by=2,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1");
    let errors = tempfile::tempfile()?;
    let mut encoder = tokio::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-f", "concat", "-i"])
        .arg(frames.path().join("frames.txt"))
        .args([
            "-an",
            "-vf",
            &fit,
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
        ])
        .arg(output.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(errors.try_clone()?)
        .kill_on_drop(true)
        .spawn()
        .context("browser recording requires ffmpeg with the libx264 encoder on PATH")?;
    let status = timeout(Duration::from_secs(30), encoder.wait()).await??;
    if !status.success() {
        use std::io::{Read, Seek};
        let mut errors = errors;
        errors.rewind()?;
        let mut message = String::new();
        errors.take(8192).read_to_string(&mut message)?;
        anyhow::bail!("browser recording encoder failed: {message}");
    }
    output
        .persist_noclobber(&path)
        .context("could not save recording without replacing an existing file")?;
    Ok(
        serde_json::json!({"path": path, "surface": surface, "frames": captured.len(), "seconds": elapsed.as_secs_f64()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_ipc::protocol::{Envelope, Payload, RpcError, TreePane, TreeTab, TreeWorkspace};
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixListener,
    };

    #[tokio::test]
    async fn recording_pins_each_capture_and_cleans_up_failure_or_cancellation() {
        for mode in ["not a PNG", "pixel budget", "tab closed", "cancelled"] {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("ipc");
            let listener = UnixListener::bind(&socket).unwrap();
            let pane = PaneId::new();
            let surface = SurfaceId::new();
            let mut server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let mut frame_path = PathBuf::new();
                for index in 0..if mode == "cancelled" { 2 } else { 3 } {
                    let mut line = String::new();
                    reader.read_line(&mut line).await.unwrap();
                    let request: Envelope = serde_json::from_str(&line).unwrap();
                    let response = if index == 0 {
                        assert!(matches!(
                            request.payload,
                            Payload::Request(Request::WorkspaceTree)
                        ));
                        Response::Tree {
                            workspaces: vec![TreeWorkspace {
                                id: WorkspaceId::new(),
                                name: "test".into(),
                                root: None,
                                location: flowmux_core::WorkspaceLocation::Local {
                                    root_dir: "/tmp".into(),
                                },
                                panes: vec![TreePane {
                                    id: pane,
                                    tabs: vec![TreeTab {
                                        id: surface,
                                        title: "test".into(),
                                        kind: "browser".into(),
                                        active: true,
                                        agent: None,
                                    }],
                                }],
                            }],
                        }
                    } else {
                        let Payload::Request(Request::BrowserOnSurface {
                            surface: actual,
                            request,
                        }) = request.payload
                        else {
                            panic!("capture was not pinned")
                        };
                        assert_eq!(actual, surface);
                        let Request::BrowserScreenshot {
                            pane: actual,
                            path,
                            full_page,
                            element,
                        } = *request
                        else {
                            panic!("expected viewport capture")
                        };
                        assert_eq!(actual, pane);
                        assert!(!full_page && element.is_none());
                        frame_path = path;
                        if index == 2 && mode == "tab closed" {
                            Response::Error(RpcError::InvalidArgument("tab closed".into()))
                        } else {
                            let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
                            let width = if index == 2 && mode == "pixel budget" {
                                u32::MAX
                            } else {
                                index as u32
                            };
                            png.extend_from_slice(&width.to_be_bytes());
                            png.extend_from_slice(&1u32.to_be_bytes());
                            if index == 2 && mode == "not a PNG" {
                                png.clear();
                            }
                            std::fs::write(&frame_path, png).unwrap();
                            Response::BrowserResult {
                                value: frame_path.display().to_string(),
                            }
                        }
                    };
                    let answer = Envelope {
                        id: request.id,
                        payload: Payload::Response(response),
                    };
                    write
                        .write_all(
                            format!("{}\n", serde_json::to_string(&answer).unwrap()).as_bytes(),
                        )
                        .await
                        .unwrap();
                }
                frame_path
            });
            let client = Client::connect(&socket).await.unwrap();
            let output = directory.path().join("result.mp4");
            let mut recording = Box::pin(record(&client, pane, None, output.clone(), 1, 10));
            let frame_path = if mode == "cancelled" {
                let frame = tokio::select! {
                    frame = &mut server => frame.unwrap(),
                    result = &mut recording => panic!("recording ended before cancellation: {result:?}"),
                };
                drop(recording);
                frame
            } else {
                let error = recording.await.unwrap_err();
                assert!(error.to_string().contains(mode), "{error}");
                server.await.unwrap()
            };
            assert!(!output.exists());
            assert!(
                !frame_path.parent().unwrap().exists(),
                "temporary frames leaked"
            );
            assert_eq!(
                std::fs::read_dir(directory.path()).unwrap().count(),
                1,
                "temporary output leaked"
            );
        }
    }
}
