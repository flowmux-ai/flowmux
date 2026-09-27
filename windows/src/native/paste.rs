// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

impl App {
    pub(super) fn begin_paste(
        &mut self,
        id: SurfaceId,
        text: String,
        reply: ipc::Reply,
    ) -> anyhow::Result<()> {
        crate::paste::validate(&text)?;
        let session = self.session(id)?;
        let surface = &self.surfaces[&id];
        anyhow::ensure!(surface.ready && !surface.restoring, "terminal is not ready");
        anyhow::ensure!(self.pending_pastes.len() < 16, "too many pending pastes");
        anyhow::ensure!(
            !self.pending_pastes.values().any(|p| p.surface == id),
            "another paste is pending for this terminal"
        );
        // Includes output queued by the ConPTY reader but not yet delivered to
        // the WebView. The frontend must parse its mode changes before pasting.
        let after = session.barrier();
        let request = Uuid::new_v4();
        surface.send(&HostMessage::Paste {
            request,
            after,
            text,
        })?;
        self.pending_pastes.insert(
            request,
            PendingRead {
                surface: id,
                after,
                reply,
                started: Instant::now(),
            },
        );
        Ok(())
    }

    pub(super) fn pasted(
        &mut self,
        id: SurfaceId,
        request: Option<Uuid>,
        sequence: u64,
        outcome: crate::paste::Outcome,
    ) -> anyhow::Result<()> {
        let pending = if let Some(request) = request {
            let Some(pending) = self.pending_pastes.get(&request) else {
                // Timed-out, closed or duplicate replies must never inject input.
                return Ok(());
            };
            anyhow::ensure!(pending.surface == id, "foreign paste response");
            self.pending_pastes.remove(&request)
        } else {
            None
        };
        let result = (|| -> anyhow::Result<Value> {
            if let Some(pending) = &pending {
                anyhow::ensure!(
                    pending.started.elapsed() <= Duration::from_secs(12),
                    "paste request expired before input was queued"
                );
                anyhow::ensure!(
                    sequence >= pending.after,
                    "paste preceded terminal mode parsing"
                );
            }
            let surface = &self.surfaces[&id];
            anyhow::ensure!(
                sequence <= surface.output_sequence,
                "invalid paste sequence"
            );
            anyhow::ensure!(
                self.close_request.is_none(),
                "window is saving before close"
            );
            anyhow::ensure!(surface.ready && !surface.restoring, "terminal is not ready");
            let session = self.session(id)?;
            let (bytes, bracketed) = outcome.into_input()?;
            let accepted_bytes = bytes.len();
            if !bytes.is_empty() {
                session.input(bytes)?;
            }
            Ok(
                json!({"surface":id,"sequence":sequence,"accepted_bytes":accepted_bytes,
                "bracketed":bracketed,"delivery":"queued"}),
            )
        })();
        if let Some(pending) = pending {
            let _ = pending
                .reply
                .try_send(result.unwrap_or_else(|error| json!({"error":error.to_string()})));
        } else {
            self.surfaces[&id].send(&HostMessage::PasteResult {
                error: result.err().map(|error| error.to_string()),
            })?;
        }
        Ok(())
    }
}
