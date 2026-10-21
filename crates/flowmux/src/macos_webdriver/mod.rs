// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in W3C endpoint, isolated from the normal application's state and windows.
mod capabilities;
mod native;
mod session;

use std::net::SocketAddr;
use std::sync::mpsc;
use webdriver::command::WebDriverMessage;
use webdriver::error::{ErrorStatus, WebDriverError, WebDriverResult};
use webdriver::response::WebDriverResponse;
use webdriver::server::{Session, SessionTeardownKind, WebDriverHandler};

type Request = (
    WebDriverMessage,
    mpsc::Sender<WebDriverResult<WebDriverResponse>>,
);
struct Bridge(mpsc::Sender<Request>);

impl WebDriverHandler for Bridge {
    fn handle_command(
        &mut self,
        _session: &Option<Session>,
        message: WebDriverMessage,
    ) -> WebDriverResult<WebDriverResponse> {
        let (done, result) = mpsc::channel();
        self.0
            .send((message, done))
            .map_err(|_| WebDriverError::new(ErrorStatus::UnknownError, "browser stopped"))?;
        result
            .recv()
            .map_err(|_| WebDriverError::new(ErrorStatus::UnknownError, "browser stopped"))?
    }
    fn teardown_session(&mut self, _kind: SessionTeardownKind) {}
}

fn address(value: &str) -> anyhow::Result<SocketAddr> {
    let address: SocketAddr = value.parse()?;
    anyhow::ensure!(
        address.ip().is_loopback() && address.port() != 0,
        "FLOWMUX_WEBDRIVER_SERVER requires a numeric loopback address and a nonzero port"
    );
    Ok(address)
}

pub(crate) fn run(value: &str) -> anyhow::Result<()> {
    let address = address(value)?;
    let _app = native::application();
    let (requests, incoming) = mpsc::channel();
    // Mozilla's server validates Host/Origin, request types, routes and session IDs.
    let listener = webdriver::server::start(address, vec![], vec![], Bridge(requests), vec![])?;
    let (stopped, shutdown) = mpsc::channel();
    std::thread::spawn(move || {
        drop(listener);
        let _ = stopped.send(());
    });
    println!("Flowmux WebDriver listening on http://{address}");
    let mut state = session::Driver::default();
    while shutdown.try_recv().is_err() {
        if let Ok((message, reply)) = incoming.try_recv() {
            let result = state.handle(message.command);
            let _ = reply.send(result);
        }
        native::pump();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn driver_listener_is_explicit_and_loopback_only() {
        for value in ["127.0.0.1:4444", "[::1]:4444"] {
            assert!(super::address(value).is_ok());
        }
        for value in [
            "",
            "localhost:4444",
            "0.0.0.0:4444",
            "[::]:4444",
            "127.0.0.1:0",
        ] {
            assert!(super::address(value).is_err(), "{value}");
        }
    }
}
