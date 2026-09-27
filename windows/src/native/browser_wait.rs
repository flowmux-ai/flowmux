// SPDX-License-Identifier: GPL-3.0-or-later
//! One in-flight WebView callback per wait, pinned to the initial surface.
use super::*;
pub(in crate::native::host) const TIMER: usize = 2;
pub(in crate::native::host) struct Wait {
    surface: SurfaceId,
    source: String,
    reply: ipc::Reply,
    deadline: Instant,
    next: Instant,
    interval: Duration,
    during_load: bool,
    flight: Option<(Uuid, u64, Instant)>,
}
impl App {
    pub(super) fn browser_wait_start(
        &mut self,
        surface: SurfaceId,
        options: crate::browser_wait::Options,
        reply: ipc::Reply,
    ) -> anyhow::Result<()> {
        let source = options.script()?;
        anyhow::ensure!(
            self.browser_waits.len() < 8,
            "at most eight browser waits may be pending"
        );
        let now = Instant::now();
        self.browser_waits.insert(
            Uuid::new_v4(),
            Wait {
                surface,
                source,
                reply,
                deadline: now + Duration::from_millis(options.timeout_ms),
                next: now,
                interval: Duration::from_millis(options.poll_ms),
                during_load: options.during_load(),
                flight: None,
            },
        );
        self.browser_wait_tick();
        Ok(())
    }
    pub(super) fn browser_wait_cancel(&mut self, surface: SurfaceId, reason: &str) {
        self.browser_waits.retain(|_, wait| {
            if wait.surface != surface {
                return true;
            }
            let _ = wait.reply.try_send(json!({"error":reason}));
            false
        });
        self.browser_wait_schedule();
    }
    fn browser_wait_schedule(&mut self) {
        // This timer does not change the existing one-second terminal/state tick.
        unsafe {
            KillTimer(self.window, TIMER);
        }
        let next = self
            .browser_waits
            .values()
            .map(|wait| {
                wait.deadline.min(
                    wait.flight
                        .map(|(_, _, start)| start + Duration::from_secs(12))
                        .unwrap_or(wait.next),
                )
            })
            .min();
        if let Some(next) = next {
            let millis = next
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(10, u32::MAX as u128) as u32;
            if unsafe { SetTimer(self.window, TIMER, millis, None) } == 0 {
                for (_, wait) in self.browser_waits.drain() {
                    let _ = wait
                        .reply
                        .try_send(json!({"error":"could not schedule browser wait"}));
                }
            }
        }
    }
    pub(super) fn browser_wait_tick(&mut self) {
        for (id, mut wait) in std::mem::take(&mut self.browser_waits) {
            let now = Instant::now();
            if now >= wait.deadline {
                let _ = wait
                    .reply
                    .try_send(json!({"result":false,"surface":wait.surface}));
                continue;
            }
            let Some(browser) = self.browsers.get(&wait.surface) else {
                let _ = wait
                    .reply
                    .try_send(json!({"error":"browser closed during wait"}));
                continue;
            };
            if let Some((_, _, start)) = wait.flight {
                if now.duration_since(start) >= Duration::from_secs(12) {
                    let _=wait.reply.try_send(json!({"error":"browser wait callback timed out; predicate may have executed"}));
                    continue;
                }
            } else if now >= wait.next {
                if browser.loading && !wait.during_load {
                    wait.next = now + wait.interval;
                } else {
                    let poll = Uuid::new_v4();
                    let epoch = browser.epoch.load(Ordering::SeqCst);
                    let sender = self.sender.clone();
                    let source = serde_json::to_string(&wait.source).expect("string serialization");
                    let script=format!("(()=>{{try{{return {{result:(0,eval)({source})}};}}catch(e){{return {{error:String(e).slice(0,4096)}};}}}})()");
                    match browser
                        .view
                        .evaluate_script_with_callback(&script, move |result| {
                            sender.send(Event::Browser(Signal::WaitResult(
                                id,
                                poll,
                                epoch,
                                if result.len() > 16 * 1024 {
                                    "{\"error\":\"wait response exceeds limit\"}".into()
                                } else {
                                    result
                                },
                            )))
                        }) {
                        Ok(()) => wait.flight = Some((poll, epoch, now)),
                        Err(error) => {
                            let _ = wait.reply.try_send(json!({"error":error.to_string()}));
                            continue;
                        }
                    }
                }
            }
            self.browser_waits.insert(id, wait);
        }
        self.browser_wait_schedule();
    }
    pub(super) fn browser_wait_result(&mut self, id: Uuid, poll: Uuid, epoch: u64, result: String) {
        let Some(mut wait) = self.browser_waits.remove(&id) else {
            return;
        };
        if wait.flight.is_none_or(|(current, _, _)| current != poll) {
            self.browser_waits.insert(id, wait);
            return;
        }
        let now = Instant::now();
        let reply = if now >= wait.deadline {
            Some(json!({"result":false,"surface":wait.surface}))
        } else if wait
            .flight
            .is_some_and(|(_, _, start)| now.duration_since(start) >= Duration::from_secs(12))
        {
            Some(json!({"error":"browser wait callback timed out; predicate may have executed"}))
        } else if let Some(browser) = self.browsers.get(&wait.surface) {
            if browser.epoch.load(Ordering::SeqCst) != epoch
                || (browser.loading && !wait.during_load)
            {
                None
            } else {
                match serde_json::from_str::<Value>(&result) {
                    Ok(value) if value["error"].is_string() => {
                        Some(json!({"error":value["error"]}))
                    }
                    Ok(value) if value["result"] == true => {
                        Some(json!({"result":true,"surface":wait.surface}))
                    }
                    Ok(value) if value["result"] == false => None,
                    _ => Some(json!({"error":"invalid browser wait response"})),
                }
            }
        } else {
            Some(json!({"error":"browser closed during wait"}))
        };
        if let Some(reply) = reply {
            let _ = wait.reply.try_send(reply);
        } else {
            wait.flight = None;
            wait.next = now + wait.interval;
            self.browser_waits.insert(id, wait);
        }
        self.browser_wait_schedule();
    }
}
