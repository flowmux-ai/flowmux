// SPDX-License-Identifier: GPL-3.0-or-later
//! Native viewport capture. COM stays on the UI thread; disk I/O does not.
use super::*;
use crate::browser_capture::{dimensions, png_dimensions, MAX_BYTES};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Component, Path, Prefix},
};
use webview2_com::{
    CapturePreviewCompletedHandler,
    Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
};
use windows::Win32::System::Com::{
    IStream, StructuredStorage::CreateStreamOnHGlobal, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET,
};
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

const DEADLINE: Duration = Duration::from_secs(12);
pub(in super::super) struct Pending {
    surface: SurfaceId,
    epoch: u64,
    revision: u64,
    size: (u32, u32),
    path: PathBuf,
    bytes: usize,
    writing: bool,
    reply: Option<ipc::Reply>,
    started: Instant,
}
impl Pending {
    fn reject(&mut self, reason: &str) {
        if let Some(reply) = self.reply.take() {
            let suffix = if self.writing {
                "; file save may still complete (not retried)"
            } else {
                "; no file save was started"
            };
            let _ = reply.try_send(json!({"error":format!("{reason}{suffix}")}));
        }
    }
}

fn path_valid(path: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(path.is_absolute(), "screenshot IPC path must be absolute");
    let prefix = path.components().next();
    anyhow::ensure!(
        matches!(prefix, Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_) | Prefix::UNC(..) | Prefix::VerbatimUNC(..))),
        "screenshot requires an ordinary filesystem path"
    );
    let text = path.to_str().context("screenshot path is not Unicode")?;
    anyhow::ensure!(
        !text.chars().any(char::is_control) && text.encode_utf16().count() < 32767,
        "invalid screenshot path"
    );
    for part in path.components() {
        if let Component::Normal(name) = part {
            let name = name.to_str().context("screenshot path is not Unicode")?;
            let stem = name
                .split('.')
                .next()
                .unwrap_or("")
                .trim_end()
                .to_uppercase();
            let numbered_device = ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            });
            anyhow::ensure!(
                !matches!(
                    stem.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                ) && !numbered_device,
                "screenshot path contains a reserved device name"
            );
            anyhow::ensure!(
                !name.contains([':', '*', '?', '"', '<', '>', '|']) && !name.ends_with(['.', ' ']),
                "invalid screenshot path component"
            );
        }
    }
    anyhow::ensure!(
        path.file_name().is_some()
            && path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png")),
        "screenshot destination must have a .png extension"
    );
    Ok(())
}

fn save(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    path_valid(path)?;
    let parent = path
        .parent()
        .context("missing screenshot parent directory")?
        .canonicalize()
        .context("screenshot parent directory must exist")?;
    let destination = parent.join(path.file_name().context("missing screenshot filename")?);
    let temporary = parent.join(format!(".flowmux-capture-{}.tmp", Uuid::new_v4()));
    // Only remove a temp file which this invocation successfully created.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .context("creating screenshot temporary file")?;
    let result = (|| -> anyhow::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        unsafe {
            checked(MoveFileExW(
                wide(&temporary).as_ptr(),
                wide(&destination).as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            ))?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.context("saving screenshot by atomic replacement")
}

fn read_stream(stream: &IStream) -> anyhow::Result<Vec<u8>> {
    unsafe {
        let mut stat = STATSTG::default();
        stream.Stat(&mut stat, STATFLAG_NONAME)?;
        anyhow::ensure!(
            stat.cbSize > 0 && stat.cbSize <= MAX_BYTES as u64,
            "screenshot stream is empty or exceeds 32 MiB"
        );
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut bytes = vec![0; stat.cbSize as usize];
        let mut read = 0;
        stream
            .Read(
                bytes.as_mut_ptr().cast(),
                bytes.len() as u32,
                Some(&mut read),
            )
            .ok()?;
        anyhow::ensure!(read as usize == bytes.len(), "incomplete screenshot stream");
        Ok(bytes)
    }
}
fn viewport(browser: &Browser) -> anyhow::Result<(u32, u32)> {
    let mut bounds = windows::Win32::Foundation::RECT::default();
    unsafe {
        browser.view.controller().Bounds(&mut bounds)?;
    }
    let width = u32::try_from(i64::from(bounds.right) - i64::from(bounds.left))?;
    let height = u32::try_from(i64::from(bounds.bottom) - i64::from(bounds.top))?;
    dimensions(width, height)?;
    Ok((width, height))
}

impl App {
    pub(super) fn browser_capture_start(
        &mut self,
        id: SurfaceId,
        path: PathBuf,
        reply: ipc::Reply,
    ) -> anyhow::Result<()> {
        path_valid(&path)?;
        anyhow::ensure!(
            self.pending_captures.len() < 2,
            "two browser captures or file saves are already outstanding"
        );
        let browser = self.browsers.get(&id).context("browser was closed")?;
        anyhow::ensure!(browser.visible, "screenshot requires a visible browser tab");
        anyhow::ensure!(
            !browser.loading && browser.error.is_none(),
            "wait for successful browser navigation before taking a screenshot"
        );
        let size = viewport(browser)?;
        let request = Uuid::new_v4();
        let stream = unsafe { CreateStreamOnHGlobal(Default::default(), true)? };
        let completed_stream = stream.clone();
        let sender = self.sender.clone();
        let callback = CapturePreviewCompletedHandler::create(Box::new(move |status| {
            let result = status
                .map_err(anyhow::Error::from)
                .and_then(|()| read_stream(&completed_stream));
            sender.send(Event::Browser(Signal::Capture(
                request,
                result.map_err(|error| format!("{error:#}")),
            )));
            Ok(())
        }));
        let pending = Pending {
            surface: id,
            epoch: browser.epoch.load(Ordering::SeqCst),
            revision: browser.viewport_revision,
            size,
            path,
            bytes: 0,
            writing: false,
            reply: Some(reply),
            started: Instant::now(),
        };
        unsafe {
            browser.view.controller().CoreWebView2()?.CapturePreview(
                COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                &stream,
                &callback,
            )?;
        }
        self.pending_captures.insert(request, pending);
        Ok(())
    }
    pub(super) fn browser_capture_result(&mut self, id: Uuid, result: Result<Vec<u8>, String>) {
        let Some(mut pending) = self.pending_captures.remove(&id) else {
            return;
        };
        if pending.reply.is_none() {
            return;
        }
        let result = (|| -> anyhow::Result<Vec<u8>> {
            anyhow::ensure!(
                pending.started.elapsed() <= DEADLINE,
                "browser capture callback timed out"
            );
            let browser = self
                .browsers
                .get(&pending.surface)
                .context("browser was closed during capture")?;
            anyhow::ensure!(
                browser.visible
                    && !browser.loading
                    && browser.epoch.load(Ordering::SeqCst) == pending.epoch
                    && browser.viewport_revision == pending.revision
                    && viewport(browser)? == pending.size,
                "browser document or viewport changed during capture"
            );
            let bytes = result.map_err(anyhow::Error::msg)?;
            anyhow::ensure!(
                png_dimensions(&bytes)? == pending.size,
                "captured PNG dimensions differ from the viewport"
            );
            Ok(bytes)
        })();
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(error) => {
                pending.reject(&format!("{error:#}"));
                return;
            }
        };
        // The validated image now describes this surface/epoch. Later navigation
        // cannot change those bytes and need not cancel the authorized file save.
        pending.bytes = bytes.len();
        let path = pending.path.clone();
        let sender = self.sender.clone();
        let worker = std::thread::Builder::new()
            .name("browser-png-save".into())
            .spawn(move || {
                let result = save(&path, &bytes).map_err(|error| format!("{error:#}"));
                sender.send(Event::Browser(Signal::CaptureSaved(id, result)));
            });
        match worker {
            Ok(_) => {
                pending.writing = true;
                self.pending_captures.insert(id, pending);
            }
            Err(error) => {
                pending.reject(&format!("could not start screenshot file writer: {error}"))
            }
        }
    }
    pub(super) fn browser_capture_saved(&mut self, id: Uuid, result: Result<(), String>) {
        if let Some(mut pending) = self.pending_captures.remove(&id) {
            if pending.started.elapsed() > DEADLINE {
                pending.reject("browser screenshot timed out");
            }
            if let Some(reply) = pending.reply.take() {
                let value = match result {
                    Ok(()) => {
                        json!({"path":pending.path,"surface":pending.surface,"generation":pending.epoch,"width":pending.size.0,"height":pending.size.1,"bytes":pending.bytes})
                    }
                    Err(error) => json!({"error":error}),
                };
                let _ = reply.try_send(value);
            }
        }
    }
    pub(super) fn browser_capture_tick(&mut self) {
        for pending in self.pending_captures.values_mut() {
            if pending.started.elapsed() > DEADLINE {
                pending.reject("browser screenshot timed out");
            }
        }
        // Keep timed-out slots until their real callback/writer completes. This
        // bounds outstanding native captures and stalled filesystem workers too.
    }
    pub(super) fn browser_capture_cancel(&mut self, surface: SurfaceId, reason: &str) {
        for pending in self
            .pending_captures
            .values_mut()
            .filter(|p| p.surface == surface)
        {
            pending.reject(reason);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_capture_paths_reject_streams_devices_and_relative_raw_ipc() {
        for path in [
            r"C:\tmp\한 글 한 😀.PNG",
            r"\\?\C:\tmp\test.png",
            r"\\server\share\test.png",
            r"\\?\UNC\server\share\test.png",
        ] {
            assert!(path_valid(Path::new(path)).is_ok(), "{path}");
        }
        for path in [
            r"test.png",
            r"C:test.png",
            r"C:\tmp\test.jpg",
            r"C:\tmp\CON.png",
            r"C:\tmp\NUL\test.png",
            r"C:\tmp\COM¹.png",
            r"C:\tmp\test.png:stream",
            r"C:\tmp\bad.\test.png",
            r"\\.\pipe\capture.png",
            r"\\?\GLOBALROOT\Device\capture.png",
            "C:\\tmp\\bad\0.png",
        ] {
            assert!(path_valid(Path::new(path)).is_err(), "{path}");
        }
    }
    #[test]
    fn atomic_save_preserves_locked_destination_and_removes_owned_temp() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = std::env::temp_dir().join(format!("flowmux-capture-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("한글 한 😀.png");
        save(&path, b"first").unwrap();
        let lock = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(save(&path, b"replacement").is_err());
        drop(lock);
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        save(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert!(save(&dir.join("missing").join("test.png"), b"x").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
