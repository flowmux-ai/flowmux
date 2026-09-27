// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

pub(super) struct PendingKey {
    pub(super) read: PendingRead,
    key: crate::keys::Key,
}
impl App {
    pub(super) fn begin_key(
        &mut self,
        id: SurfaceId,
        key: String,
        reply: ipc::Reply,
    ) -> anyhow::Result<()> {
        let key = crate::keys::Key::parse(&key)?;
        let session = self.session(id)?;
        let surface = &self.surfaces[&id];
        anyhow::ensure!(surface.ready && !surface.restoring, "terminal is not ready");
        anyhow::ensure!(self.pending_keys.len() < 16, "too many pending named keys");
        anyhow::ensure!(
            !self.pending_keys.values().any(|p| p.read.surface == id)
                && !self.pending_pastes.values().any(|p| p.surface == id),
            "another input request is pending for this terminal"
        );
        let after = session.barrier();
        let request = Uuid::new_v4();
        surface.send(&HostMessage::KeyMode { request, after })?;
        self.pending_keys.insert(
            request,
            PendingKey {
                read: PendingRead {
                    surface: id,
                    after,
                    reply,
                    started: Instant::now(),
                },
                key,
            },
        );
        Ok(())
    }
    pub(super) fn key_mode(
        &mut self,
        id: SurfaceId,
        request: Uuid,
        sequence: u64,
        outcome: crate::keys::ModeOutcome,
    ) -> anyhow::Result<()> {
        let Some(pending) = self.pending_keys.get(&request) else {
            return Ok(());
        };
        anyhow::ensure!(pending.read.surface == id, "foreign named-key response");
        let pending = self.pending_keys.remove(&request).unwrap();
        let result = (|| -> anyhow::Result<Value> {
            anyhow::ensure!(
                pending.read.started.elapsed() <= Duration::from_secs(12),
                "named-key request expired before input was queued"
            );
            anyhow::ensure!(
                sequence >= pending.read.after,
                "named key preceded terminal mode parsing"
            );
            let surface = &self.surfaces[&id];
            anyhow::ensure!(
                sequence <= surface.output_sequence,
                "invalid named-key sequence"
            );
            anyhow::ensure!(
                self.close_request.is_none(),
                "window is saving before close"
            );
            anyhow::ensure!(surface.ready && !surface.restoring, "terminal is not ready");
            let application_cursor = outcome.application_cursor()?;
            let bytes = pending.key.encode(application_cursor)?;
            let accepted_bytes = bytes.len();
            self.session(id)?.input(bytes)?;
            Ok(
                json!({"ok":true,"surface":id,"sequence":sequence,"accepted_bytes":accepted_bytes,
                "application_cursor":application_cursor,"delivery":"queued"}),
            )
        })();
        let _ = pending
            .read
            .reply
            .try_send(result.unwrap_or_else(|e| json!({"error":e.to_string()})));
        Ok(())
    }
}
