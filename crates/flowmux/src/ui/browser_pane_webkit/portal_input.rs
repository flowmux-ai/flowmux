// SPDX-License-Identifier: GPL-3.0-or-later
//! Wayland input uses the compositor's consent-based RemoteDesktop portal.
use super::input::Input;
use flowmux_browser::input::{PointerAction, PointerTarget};
use gtk::{gio, glib, prelude::*};
use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

const SERVICE: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.RemoteDesktop";
type Options = HashMap<String, glib::Variant>;

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Requesting,
    Ready(Rc<Session>),
    Failed(String),
}

#[derive(Default)]
enum Capability {
    #[default]
    Unknown,
    Checking,
    Available,
    Unavailable(String),
}

thread_local! {
    static CAPABILITY: RefCell<Capability> = const { RefCell::new(Capability::Unknown) };
    static STATE: RefCell<State> = const { RefCell::new(State::Idle) };
}

struct Session {
    bus: gio::DBusConnection,
    path: glib::variant::ObjectPath,
    subscriptions: Vec<gio::SignalSubscriptionId>,
}

impl Drop for Session {
    fn drop(&mut self) {
        for id in self.subscriptions.drain(..) {
            self.bus.signal_unsubscribe(id);
        }
        self.bus.call(
            Some(SERVICE),
            self.path.as_str(),
            "org.freedesktop.portal.Session",
            "Close",
            None,
            None,
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
            |_| {},
        );
    }
}

fn invalidate(path: &glib::variant::ObjectPath, error: String) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if matches!(&*state, State::Ready(session) if &session.path == path) {
            *state = State::Failed(error);
        }
    });
}

pub(super) fn supported() -> bool {
    CAPABILITY.with(|capability| matches!(*capability.borrow(), Capability::Available))
}

pub(super) fn error() -> Option<String> {
    STATE.with(|state| match &*state.borrow() {
        State::Failed(error) => Some(error.clone()),
        _ => CAPABILITY.with(|capability| match &*capability.borrow() {
            Capability::Unavailable(error) => Some(error.clone()),
            _ => None,
        }),
    })
}

async fn check_capability() -> Result<(), String> {
    let bus = gio::bus_get_future(gio::BusType::Session)
        .await
        .map_err(|e| e.to_string())?;
    let reply = bus
        .call_future(
            Some(SERVICE),
            PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(INTERFACE, "AvailableDeviceTypes").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            3000,
        )
        .await
        .map_err(|e| format!("RemoteDesktop portal unavailable: {e}"))?;
    let devices = reply
        .get::<(glib::Variant,)>()
        .and_then(|(value,)| value.get::<u32>())
        .unwrap_or(0);
    if devices & 3 != 3 {
        return Err("RemoteDesktop portal does not support both keyboard and pointer input".into());
    }
    Ok(())
}

pub(super) fn reason() -> Option<&'static str> {
    CAPABILITY.with(|capability| {
        let mut capability = capability.borrow_mut();
        if matches!(*capability, Capability::Unknown) {
            *capability = Capability::Checking;
            glib::spawn_future_local(async {
                let result = check_capability().await;
                CAPABILITY.with(|capability| {
                    *capability.borrow_mut() = match result {
                        Ok(()) => Capability::Available,
                        Err(error) => Capability::Unavailable(error),
                    }
                });
            });
        }
    });
    STATE.with(|state| match &*state.borrow() {
        State::Ready(_) => None,
        State::Requesting => Some("wayland_permission_pending"),
        State::Failed(_) => Some("wayland_portal_failed"),
        State::Idle => CAPABILITY.with(|capability| {
            Some(match &*capability.borrow() {
                Capability::Available => "wayland_permission_required",
                Capability::Unavailable(_) => "wayland_portal_unavailable",
                _ => "wayland_portal_checking",
            })
        }),
    })
}

async fn request(
    bus: &gio::DBusConnection,
    method: &str,
    mut args: Vec<glib::Variant>,
    mut options: Options,
) -> Result<Options, String> {
    let token = format!("flowmux_{}", uuid::Uuid::new_v4().simple());
    options.insert("handle_token".into(), token.to_variant());
    let sender = bus
        .unique_name()
        .ok_or("portal bus has no unique name")?
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = RefCell::new(Some(tx));
    let subscription = bus.signal_subscribe(
        Some(SERVICE),
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        Some(&path),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, value| {
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(value.clone());
            }
        },
    );
    args.push(options.to_variant());
    let result = async {
        bus.call_future(
            Some(SERVICE),
            PATH,
            INTERFACE,
            method,
            Some(&glib::Variant::tuple_from_iter(args)),
            None,
            gio::DBusCallFlags::NONE,
            5000,
        )
        .await
        .map_err(|error| error.to_string())?;
        let response = glib::future_with_timeout(Duration::from_secs(120), rx)
            .await
            .map_err(|_| "Wayland input permission timed out")?
            .map_err(|_| "Wayland input permission request closed")?;
        let (code, results) = response
            .get::<(u32, Options)>()
            .ok_or("invalid portal response")?;
        if code != 0 {
            return Err("Wayland input permission was cancelled or denied".into());
        }
        Ok(results)
    }
    .await;
    bus.signal_unsubscribe(subscription);
    if result.is_err() {
        bus.call(
            Some(SERVICE),
            &path,
            "org.freedesktop.portal.Request",
            "Close",
            None,
            None,
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
            |_| {},
        );
    }
    result
}

async fn connect() -> Result<Rc<Session>, String> {
    let bus = gio::bus_get_future(gio::BusType::Session)
        .await
        .map_err(|error| error.to_string())?;
    let options = [(
        "session_handle_token".into(),
        format!("flowmux_{}", uuid::Uuid::new_v4().simple()).to_variant(),
    )]
    .into();
    let created = request(&bus, "CreateSession", vec![], options).await?;
    let path = created
        .get("session_handle")
        .and_then(|value| value.str())
        .ok_or("portal did not return a session")?;
    let mut session = Session {
        bus,
        path: glib::variant::ObjectPath::try_from(path).map_err(|error| error.to_string())?,
        subscriptions: Vec::new(),
    };
    request(
        &session.bus,
        "SelectDevices",
        vec![session.path.to_variant()],
        [("types".into(), 3u32.to_variant())].into(),
    )
    .await?;
    let result = request(
        &session.bus,
        "Start",
        vec![session.path.to_variant(), "".to_variant()],
        Options::new(),
    )
    .await?;
    if result
        .get("devices")
        .and_then(|value| value.get::<u32>())
        .is_none_or(|devices| devices & 3 != 3)
    {
        return Err("Wayland input requires both keyboard and pointer permission".into());
    }
    let path = session.path.clone();
    session.subscriptions.push(session.bus.signal_subscribe(
        Some(SERVICE),
        Some("org.freedesktop.portal.Session"),
        Some("Closed"),
        Some(session.path.as_str()),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, _| {
            invalidate(&path, "Wayland input permission was revoked".into());
        },
    ));
    let path = session.path.clone();
    session.subscriptions.push(session.bus.signal_subscribe(
        Some("org.freedesktop.DBus"),
        Some("org.freedesktop.DBus"),
        Some("NameOwnerChanged"),
        Some("/org/freedesktop/DBus"),
        Some(SERVICE),
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, _| {
            invalidate(&path, "Wayland input portal disconnected".into());
        },
    ));
    Ok(Rc::new(session))
}

fn session() -> Result<Rc<Session>, String> {
    if matches!(reason(), Some("wayland_portal_unavailable")) {
        let error = error().unwrap_or_default();
        CAPABILITY.with(|capability| *capability.borrow_mut() = Capability::Unknown);
        return Err(format!("{error}; install or enable a RemoteDesktop portal backend, then retry; no input was sent"));
    }
    if !supported() {
        return Err("Checking RemoteDesktop portal support; retry after browser diagnose reports permission required; no input was sent".into());
    }
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        match &*state {
            State::Ready(session) => return Ok(session.clone()),
            State::Requesting => return Err("Approve the Flowmux remote input request in the desktop dialog, then retry; no input was sent".into()),
            State::Failed(error) => {
                let error = error.clone();
                *state = State::Idle;
                return Err(format!("{error}; retry explicitly to request permission again"));
            }
            State::Idle => {}
        }
        *state = State::Requesting;
        glib::spawn_future_local(async {
            let result = connect().await;
            STATE.with(|state| *state.borrow_mut() = match result {
                Ok(session) => State::Ready(session),
                Err(error) => State::Failed(error),
            });
        });
        Err("Wayland input needs desktop permission. Approve the Flowmux remote input request, then retry; no input was sent".into())
    })
}

impl Session {
    fn notify(
        &self,
        method: &str,
        args: impl IntoIterator<Item = glib::Variant>,
    ) -> Result<(), String> {
        let args = [self.path.to_variant(), Options::new().to_variant()]
            .into_iter()
            .chain(args);
        self.bus
            .call_sync(
                Some(SERVICE),
                PATH,
                INTERFACE,
                method,
                Some(&glib::Variant::tuple_from_iter(args)),
                None,
                gio::DBusCallFlags::NONE,
                1000,
                gio::Cancellable::NONE,
            )
            .map(|_| ())
            .map_err(|error| {
                let error = error.to_string();
                invalidate(&self.path, error.clone());
                error
            })
    }
}

pub(super) fn send(view: &webkit6::WebView, input: Input<'_>) -> Result<(), String> {
    let session = session()?;
    let native = view.native().ok_or("browser has no native window")?;
    let surface = native.surface().ok_or("browser has no native surface")?;
    let toplevel = surface
        .downcast_ref::<gtk::gdk::Toplevel>()
        .ok_or("browser has no toplevel")?;
    if !toplevel.state().contains(gtk::gdk::ToplevelState::FOCUSED) {
        return Err("Focus the Flowmux window before Wayland input; no input was sent".into());
    }
    let seat = view
        .display()
        .default_seat()
        .ok_or("Wayland seat is unavailable")?;
    let held = gtk::gdk::ModifierType::SHIFT_MASK
        | gtk::gdk::ModifierType::CONTROL_MASK
        | gtk::gdk::ModifierType::ALT_MASK
        | gtk::gdk::ModifierType::SUPER_MASK
        | gtk::gdk::ModifierType::BUTTON1_MASK
        | gtk::gdk::ModifierType::BUTTON2_MASK
        | gtk::gdk::ModifierType::BUTTON3_MASK
        | gtk::gdk::ModifierType::BUTTON4_MASK
        | gtk::gdk::ModifierType::BUTTON5_MASK;
    if seat
        .keyboard()
        .into_iter()
        .chain(seat.pointer())
        .any(|device| device.modifier_state().intersects(held))
    {
        return Err("Release held keys and pointer buttons before Wayland input".into());
    }
    match input {
        Input::Key(stroke) => {
            if seat.keyboard().is_some_and(|keyboard| {
                keyboard
                    .modifier_state()
                    .contains(gtk::gdk::ModifierType::LOCK_MASK)
            }) {
                return Err(
                    "Turn off Caps Lock before Wayland key input; no input was sent".into(),
                );
            }
            if !view.grab_focus() {
                return Err("browser could not acquire keyboard focus".into());
            }
            let key = match stroke.key.as_str() {
                "Enter" => "Return",
                "Backspace" => "BackSpace",
                "ArrowLeft" => "Left",
                "ArrowRight" => "Right",
                "ArrowUp" => "Up",
                "ArrowDown" => "Down",
                "PageUp" => "Page_Up",
                "PageDown" => "Page_Down",
                name => name,
            };
            let keyval = if key.chars().count() == 1 {
                unsafe {
                    glib::translate::from_glib(gtk::gdk::ffi::gdk_unicode_to_keyval(
                        stroke.event_key().chars().next().unwrap() as u32,
                    ))
                }
            } else {
                gtk::gdk::Key::from_name(key).ok_or("unknown native key")?
            };
            let mut keys = Vec::new();
            for (enabled, name) in [
                (stroke.control, "Control_L"),
                (stroke.alt, "Alt_L"),
                (stroke.shift, "Shift_L"),
                (stroke.meta, "Super_L"),
            ] {
                if enabled {
                    keys.push(gtk::gdk::Key::from_name(name).unwrap());
                }
            }
            keys.push(keyval);
            let mut pressed = Vec::new();
            let mut result = Ok(());
            for key in keys {
                use glib::translate::IntoGlib;
                let code = key.into_glib() as i32;
                // Even a failed D-Bus reply may have delivered the press.
                pressed.push(code);
                result = session.notify(
                    "NotifyKeyboardKeysym",
                    [code.to_variant(), 1u32.to_variant()],
                );
                if result.is_err() {
                    break;
                }
            }
            for code in pressed.into_iter().rev() {
                let release = session.notify(
                    "NotifyKeyboardKeysym",
                    [code.to_variant(), 0u32.to_variant()],
                );
                result = result.and(release);
            }
            result
        }
        Input::Pointer(action, target) => {
            let pointer = seat.pointer().ok_or("Wayland pointer is unavailable")?;
            let (under_pointer, current_x, current_y) = pointer.surface_at_position();
            if under_pointer.as_ref() != Some(&surface) {
                return Err("Place the pointer over the Flowmux window before Wayland pointer input; no input was sent".into());
            }
            let position = |target: &PointerTarget| -> Result<(f64, f64), String> {
                let (x, y) = target.scale(f64::from(view.width()), f64::from(view.height()))?;
                let point = view
                    .compute_point(
                        native.upcast_ref::<gtk::Widget>(),
                        &gtk::graphene::Point::new(x as f32, y as f32),
                    )
                    .ok_or("could not map browser pointer coordinates")?;
                if !native
                    .upcast_ref::<gtk::Widget>()
                    .pick(point.x().into(), point.y().into(), gtk::PickFlags::DEFAULT)
                    .is_some_and(|hit| {
                        hit == *view.upcast_ref::<gtk::Widget>() || hit.is_ancestor(view)
                    })
                {
                    return Err("browser pointer target is covered by a native widget".into());
                }
                let (sx, sy) = native.surface_transform();
                Ok((
                    (f64::from(point.x()) + sx).round(),
                    (f64::from(point.y()) + sy).round(),
                ))
            };
            let (x, y) = position(target)?;
            let end = if let PointerAction::Drag { to_x, to_y } = action {
                Some(position(&PointerTarget {
                    x: to_x,
                    y: to_y,
                    width: target.width,
                    height: target.height,
                })?)
            } else {
                None
            };
            session.notify(
                "NotifyPointerMotion",
                [(x - current_x).to_variant(), (y - current_y).to_variant()],
            )?;
            let count = match action {
                PointerAction::Hover => 0,
                PointerAction::DoubleClick => 2,
                _ => 1,
            };
            for _ in 0..count {
                let mut result = session.notify(
                    "NotifyPointerButton",
                    [272i32.to_variant(), 1u32.to_variant()],
                );
                if let Some((end_x, end_y)) = end {
                    let (mut last_x, mut last_y) = (x, y);
                    for step in 1..=16 {
                        if result.is_err() {
                            break;
                        }
                        // Some portals truncate each relative delta; round cumulative positions.
                        let fraction = f64::from(step) / 16.0;
                        let next_x = (x + (end_x - x) * fraction).round();
                        let next_y = (y + (end_y - y) * fraction).round();
                        result = session.notify(
                            "NotifyPointerMotion",
                            [
                                (next_x - last_x).to_variant(),
                                (next_y - last_y).to_variant(),
                            ],
                        );
                        (last_x, last_y) = (next_x, next_y);
                    }
                }
                let release = session.notify(
                    "NotifyPointerButton",
                    [272i32.to_variant(), 0u32.to_variant()],
                );
                result.and(release)?;
            }
            Ok(())
        }
    }
}
