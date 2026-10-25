// SPDX-License-Identifier: GPL-3.0-or-later
//! Own the request so cancellation waits for the portal backend to close it.
use gtk::{gdk, gio, glib, prelude::*};
use std::{
    collections::HashMap, ffi::CString, os::unix::ffi::OsStrExt, path::PathBuf, time::Duration,
};

const SERVICE: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.FileChooser";
type Options = HashMap<String, glib::Variant>;

pub(super) async fn connection() -> Result<Option<gio::DBusConnection>, String> {
    let Ok(bus) = gio::bus_get_future(gio::BusType::Session).await else {
        return Ok(None);
    };
    match bus
        .call_future(
            Some(SERVICE),
            PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&(INTERFACE, "version").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5000,
        )
        .await
    {
        Ok(_) => Ok(Some(bus)),
        Err(error)
            if matches!(
                gio::DBusError::remote_error(&error).as_deref(),
                Some(
                    "org.freedesktop.DBus.Error.ServiceUnknown"
                        | "org.freedesktop.DBus.Error.UnknownMethod"
                        | "org.freedesktop.DBus.Error.UnknownInterface"
                        | "org.freedesktop.DBus.Error.InvalidArgs"
                )
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(format!("file chooser portal unavailable: {error}")),
    }
}

struct Parent {
    surface: gdk::Surface,
    handle: String,
    library: libloading::Library,
}

impl Drop for Parent {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.strip_prefix("wayland:") {
            unsafe {
                type DropHandle =
                    unsafe extern "C" fn(*mut gdk::ffi::GdkSurface, *const libc::c_char);
                if let Ok(drop_handle) = self
                    .library
                    .get::<DropHandle>(b"gdk_wayland_toplevel_drop_exported_handle\0")
                {
                    drop_handle(
                        self.surface.as_ptr(),
                        CString::new(handle).unwrap().as_ptr(),
                    );
                }
            }
        }
    }
}

impl Parent {
    async fn export(window: &gtk::Window) -> Result<Self, String> {
        let surface = window
            .surface()
            .ok_or("file chooser parent has no surface")?;
        let library =
            unsafe { libloading::Library::new("libgtk-4.so.1") }.map_err(|e| e.to_string())?;
        let handle = match surface.display().type_().name() {
            "GdkX11Display" => unsafe {
                let xid = library
                    .get::<unsafe extern "C" fn(*mut gdk::ffi::GdkSurface) -> libc::c_ulong>(
                        b"gdk_x11_surface_get_xid\0",
                    )
                    .map_err(|e| e.to_string())?;
                format!("x11:{:x}", xid(surface.as_ptr()))
            },
            "GdkWaylandDisplay" => {
                type Sender = std::cell::RefCell<Option<tokio::sync::oneshot::Sender<String>>>;
                unsafe extern "C" fn exported(
                    _: *mut gdk::ffi::GdkSurface,
                    handle: *const libc::c_char,
                    data: *mut libc::c_void,
                ) {
                    let sender = &*(data as *const Sender);
                    if let Some(sender) = sender.borrow_mut().take() {
                        let handle = if handle.is_null() {
                            String::new()
                        } else {
                            std::ffi::CStr::from_ptr(handle)
                                .to_string_lossy()
                                .into_owned()
                        };
                        let _ = sender.send(handle);
                    }
                }
                unsafe extern "C" fn destroy(data: *mut libc::c_void) {
                    drop(Box::from_raw(data as *mut Sender));
                }
                type Export = unsafe extern "C" fn(
                    *mut gdk::ffi::GdkSurface,
                    unsafe extern "C" fn(
                        *mut gdk::ffi::GdkSurface,
                        *const libc::c_char,
                        *mut libc::c_void,
                    ),
                    *mut libc::c_void,
                    unsafe extern "C" fn(*mut libc::c_void),
                ) -> glib::ffi::gboolean;
                let (tx, rx) = tokio::sync::oneshot::channel::<String>();
                unsafe {
                    let export = library
                        .get::<Export>(b"gdk_wayland_toplevel_export_handle\0")
                        .map_err(|e| e.to_string())?;
                    let data = Box::into_raw(Box::new(std::cell::RefCell::new(Some(tx))))
                        as *mut libc::c_void;
                    if export(surface.as_ptr(), exported, data, destroy) == 0 {
                        destroy(data);
                        return Err("could not export file chooser parent".into());
                    }
                }
                let handle = rx.await.map_err(|_| "file chooser parent export failed")?;
                if handle.is_empty() {
                    return Err("file chooser parent export failed".into());
                }
                format!("wayland:{handle}")
            }
            _ => return Err("unsupported file chooser display".into()),
        };
        Ok(Self {
            surface,
            handle,
            library,
        })
    }
}

async fn close(
    bus: &gio::DBusConnection,
    path: &str,
    responses: &async_channel::Receiver<glib::Variant>,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    loop {
        if responses.try_recv().is_ok() {
            return Ok(());
        }
        let result = bus
            .call_future(
                Some(SERVICE),
                path,
                "org.freedesktop.portal.Request",
                "Close",
                None,
                None,
                gio::DBusCallFlags::NONE,
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .as_millis()
                    .max(1) as i32,
            )
            .await;
        match result {
            Ok(_) => return Ok(()),
            // GNOME may export the backend request after OpenFile already returned its handle.
            Err(error)
                if gio::DBusError::remote_error(&error).as_deref()
                    == Some("org.freedesktop.DBus.Error.UnknownMethod")
                    && std::time::Instant::now() < deadline =>
            {
                glib::timeout_future(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(format!("file chooser UI did not finish closing: {error}")),
        }
    }
}

pub(super) async fn open(
    bus: &gio::DBusConnection,
    window: &gtk::Window,
    dialog: &gtk::FileDialog,
    multiple: bool,
    directories: bool,
    cancellable: &gio::Cancellable,
) -> Result<Option<Vec<PathBuf>>, String> {
    if cancellable.is_cancelled() {
        return Ok(None);
    }
    let parent = Parent::export(window).await?;
    if cancellable.is_cancelled() {
        return Ok(None);
    }
    let token = format!("flowmux_{}", uuid::Uuid::new_v4().simple());
    let sender = bus
        .unique_name()
        .ok_or("portal bus has no unique name")?
        .trim_start_matches(':')
        .replace('.', "_");
    let mut path = format!("{PATH}/request/{sender}/{token}");
    let mut options: Options = [
        ("handle_token".into(), token.to_variant()),
        ("multiple".into(), multiple.to_variant()),
        ("directory".into(), directories.to_variant()),
        ("modal".into(), dialog.is_modal().to_variant()),
    ]
    .into();
    if let Some(filters) = dialog.filters() {
        let filters: Vec<_> = filters
            .iter::<gtk::FileFilter>()
            .filter_map(Result::ok)
            .map(|f| f.to_gvariant())
            .collect();
        if !filters.is_empty() {
            options.insert(
                "filters".into(),
                glib::Variant::array_from_iter_with_type(filters[0].type_(), filters.iter()),
            );
        }
    }
    if let Some(filter) = dialog.default_filter() {
        options.insert("current_filter".into(), filter.to_gvariant());
    }
    if let Some(label) = dialog.accept_label() {
        options.insert("accept_label".into(), label.to_variant());
    }
    for (key, file) in [
        ("current_folder", dialog.initial_folder()),
        ("current_file", dialog.initial_file()),
    ] {
        if let Some(path) = file.and_then(|f| f.path()) {
            let mut bytes = path.as_os_str().as_bytes().to_vec();
            bytes.push(0);
            options.insert(key.into(), bytes.to_variant());
        }
    }
    let (tx, rx) = async_channel::bounded(1);
    let subscription = bus.signal_subscribe(
        Some(SERVICE),
        Some("org.freedesktop.portal.Request"),
        Some("Response"),
        Some(&path),
        None,
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, value| {
            let _ = tx.try_send(value.clone());
        },
    );
    let result = async {
        // Do not cancel OpenFile: its returned handle is needed to close an in-flight dialog.
        let reply = bus
            .call_future(
                Some(SERVICE),
                PATH,
                INTERFACE,
                "OpenFile",
                Some(&(parent.handle.as_str(), dialog.title().as_str(), options).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await;
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                let _ = close(bus, &path, &rx).await;
                return Err(format!("file chooser could not open: {error}"));
            }
        };
        let (handle,) = reply
            .get::<(glib::variant::ObjectPath,)>()
            .ok_or("invalid file chooser handle")?;
        if handle.as_str() != path {
            path = handle.to_string();
            close(bus, &path, &rx).await?;
            return Err("portal did not honor the file chooser handle token".into());
        }
        let response = match gio::CancellableFuture::new(rx.recv(), cancellable.clone()).await {
            Ok(response) => response.map_err(|_| "file chooser response channel closed")?,
            Err(_) => {
                close(bus, &path, &rx).await?;
                return Ok(None);
            }
        };
        let (code, results) = response
            .get::<(u32, Options)>()
            .ok_or("invalid file chooser response")?;
        if code != 0 {
            return Ok(None);
        }
        let uris = results
            .get("uris")
            .and_then(|v| v.get::<Vec<String>>())
            .ok_or("file chooser returned no files")?;
        uris.iter()
            .map(|uri| {
                gio::File::for_uri(uri)
                    .path()
                    .ok_or_else(|| "file chooser returned a non-local file".to_string())
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }
    .await;
    bus.signal_unsubscribe(subscription);
    result
}
