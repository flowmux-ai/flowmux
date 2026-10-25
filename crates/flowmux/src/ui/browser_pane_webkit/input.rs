// SPDX-License-Identifier: GPL-3.0-or-later
//! XInput2 grabs keep native input confined to this browser's native window.
use flowmux_browser::input::{KeyStroke, PointerAction, PointerTarget};
use gtk::{glib::translate::*, prelude::*};
use libloading::Library;
use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void};

#[repr(C)]
struct DeviceInfo {
    id: c_int,
    name: *mut c_char,
    usage: c_int,
    attachment: c_int,
    enabled: c_int,
    classes_len: c_int,
    classes: *mut c_void,
}
#[repr(C)]
struct EventMask {
    device: c_int,
    length: c_int,
    mask: *mut u8,
}
#[repr(C)]
#[derive(Default)]
struct KeyboardState {
    group: u8,
    locked_group: u8,
    base_group: u16,
    latched_group: u16,
    mods: u8,
    base_mods: u8,
    latched_mods: u8,
    locked_mods: u8,
    compat_state: u8,
    grab_mods: u8,
    compat_grab_mods: u8,
    lookup_mods: u8,
    compat_lookup_mods: u8,
    pointer_buttons: u16,
}

type FakeKey = unsafe extern "C" fn(*mut c_void, c_uint, c_int, c_ulong) -> c_int;
type Ungrab = unsafe extern "C" fn(*mut c_void, c_int, c_ulong) -> c_int;
type DisplayCall = unsafe extern "C" fn(*mut c_void) -> c_int;
type LockModifiers = unsafe extern "C" fn(*mut c_void, c_uint, c_uint, c_uint) -> c_int;

struct Grab {
    display: *mut c_void,
    device: Option<c_int>,
    pressed: Vec<u32>,
    button_down: bool,
    fake_button: FakeKey,
    fake: FakeKey,
    ungrab: Ungrab,
    release_server: DisplayCall,
    flush: DisplayCall,
    lock_modifiers: LockModifiers,
    saved_caps_lock: Option<u8>,
}
impl Drop for Grab {
    fn drop(&mut self) {
        unsafe {
            for code in self.pressed.iter().rev() {
                (self.fake)(self.display, *code, 0, 0);
            }
            if self.button_down {
                (self.fake_button)(self.display, 1, 0, 0);
            }
            if let Some(locked) = self.saved_caps_lock {
                (self.lock_modifiers)(self.display, 0x100, 2, u32::from(locked & 2));
            }
            if let Some(keyboard) = self.device {
                (self.ungrab)(self.display, keyboard, 0);
            }
            (self.release_server)(self.display);
            (self.flush)(self.display);
        }
    }
}

unsafe fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, String> {
    unsafe {
        library
            .get::<T>(name)
            .map(|value| *value)
            .map_err(|error| error.to_string())
    }
}

pub(super) enum Input<'a> {
    Key(&'a KeyStroke),
    Pointer(PointerAction, &'a PointerTarget),
}

pub(super) fn press(view: &webkit6::WebView, stroke: &KeyStroke) -> Result<(), String> {
    send(view, Input::Key(stroke))
}

pub(super) fn pointer(
    view: &webkit6::WebView,
    action: PointerAction,
    target: &PointerTarget,
) -> Result<(), String> {
    send(view, Input::Pointer(action, target))
}

fn send(view: &webkit6::WebView, input: Input<'_>) -> Result<(), String> {
    if !view.is_mapped() {
        return Err("native input requires a visible browser tab".into());
    }
    let display = view.display();
    if display.type_().name() == "GdkWaylandDisplay" {
        return super::portal_input::send(view, input);
    }
    if display.type_().name() != "GdkX11Display" {
        return Err("native input requires an X11 or Wayland display".into());
    }
    let surface = view
        .native()
        .and_then(|native| native.surface())
        .ok_or("browser has no native surface")?;
    unsafe {
        let gtk_lib = Library::new("libgtk-4.so.1").map_err(|e| e.to_string())?;
        let xlib = Library::new("libX11.so.6").map_err(|e| e.to_string())?;
        let xi = Library::new("libXi.so.6").map_err(|e| e.to_string())?;
        let xtest = Library::new("libXtst.so.6").map_err(|e| e.to_string())?;
        let get_display: unsafe extern "C" fn(*mut gtk::gdk::ffi::GdkDisplay) -> *mut c_void =
            symbol(&gtk_lib, b"gdk_x11_display_get_xdisplay\0")?;
        let get_window: unsafe extern "C" fn(*mut gtk::gdk::ffi::GdkSurface) -> c_ulong =
            symbol(&gtk_lib, b"gdk_x11_surface_get_xid\0")?;
        let state: unsafe extern "C" fn(*mut c_void, c_uint, *mut KeyboardState) -> c_int =
            symbol(&xlib, b"XkbGetState\0")?;
        let lock_modifiers: LockModifiers = symbol(&xlib, b"XkbLockModifiers\0")?;
        let keymap: unsafe extern "C" fn(*mut c_void, *mut u8) -> c_int =
            symbol(&xlib, b"XQueryKeymap\0")?;
        let query: unsafe extern "C" fn(*mut c_void, c_int, *mut c_int) -> *mut DeviceInfo =
            symbol(&xi, b"XIQueryDevice\0")?;
        let free: unsafe extern "C" fn(*mut DeviceInfo) = symbol(&xi, b"XIFreeDeviceInfo\0")?;
        let client_pointer: unsafe extern "C" fn(*mut c_void, c_ulong, *mut c_int) -> c_int =
            symbol(&xi, b"XIGetClientPointer\0")?;
        let grab_device: unsafe extern "C" fn(
            *mut c_void,
            c_int,
            c_ulong,
            c_ulong,
            c_ulong,
            c_int,
            c_int,
            c_int,
            *mut EventMask,
        ) -> c_int = symbol(&xi, b"XIGrabDevice\0")?;
        let grab_server: DisplayCall = symbol(&xlib, b"XGrabServer\0")?;
        let focus: unsafe extern "C" fn(*mut c_void, c_ulong, c_int, c_ulong) -> c_int =
            symbol(&xlib, b"XSetInputFocus\0")?;
        let raise: unsafe extern "C" fn(*mut c_void, c_ulong) -> c_int =
            symbol(&xlib, b"XRaiseWindow\0")?;
        let fake: FakeKey = symbol(&xtest, b"XTestFakeKeyEvent\0")?;
        let fake_button: FakeKey = symbol(&xtest, b"XTestFakeButtonEvent\0")?;
        let motion: unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int, c_ulong) -> c_int =
            symbol(&xtest, b"XTestFakeMotionEvent\0")?;
        let root_window: unsafe extern "C" fn(*mut c_void) -> c_ulong =
            symbol(&xlib, b"XDefaultRootWindow\0")?;
        let translate: unsafe extern "C" fn(
            *mut c_void,
            c_ulong,
            c_ulong,
            c_int,
            c_int,
            *mut c_int,
            *mut c_int,
            *mut c_ulong,
        ) -> c_int = symbol(&xlib, b"XTranslateCoordinates\0")?;
        let ungrab: Ungrab = symbol(&xi, b"XIUngrabDevice\0")?;
        let release_server: DisplayCall = symbol(&xlib, b"XUngrabServer\0")?;
        let flush: DisplayCall = symbol(&xlib, b"XFlush\0")?;
        let dpy = get_display(display.to_glib_none().0);
        if dpy.is_null() {
            return Err("X11 display is unavailable".into());
        }
        let mut keyboard_state = KeyboardState::default();
        if state(dpy, 0x100, &mut keyboard_state) != 0 {
            return Err("could not read the keyboard layout".into());
        }
        if keyboard_state.base_mods != 0 || keyboard_state.latched_mods != 0 {
            return Err("release held keyboard modifiers before native input".into());
        }
        if keyboard_state.pointer_buttons != 0 {
            return Err("release held pointer buttons before native input".into());
        }
        let mut codes = Vec::new();
        if let Input::Key(stroke) = &input {
            let name = match stroke.key.as_str() {
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
            let key = if name.chars().count() == 1 {
                from_glib(gtk::gdk::ffi::gdk_unicode_to_keyval(
                    name.chars().next().unwrap() as u32,
                ))
            } else {
                gtk::gdk::Key::from_name(name).ok_or("unknown native key")?
            };
            let mapping = display
                .map_keyval(key)
                .and_then(|keys| {
                    keys.into_iter().find(|key| {
                        key.group() == i32::from(keyboard_state.group) && key.level() <= 1
                    })
                })
                .ok_or("key is not available in the current keyboard layout; use type for text")?;
            for (enabled, name) in [
                (stroke.shift || mapping.level() == 1, "Shift_L"),
                (stroke.control, "Control_L"),
                (stroke.alt, "Alt_L"),
                (stroke.meta, "Super_L"),
            ] {
                if enabled {
                    let key = gtk::gdk::Key::from_name(name).unwrap();
                    codes.push(
                        display
                            .map_keyval(key)
                            .and_then(|keys| keys.first().map(|key| key.keycode()))
                            .ok_or("modifier is not mapped")?,
                    );
                }
            }
            codes.push(mapping.keycode());
        }
        let mut down = [0u8; 32];
        keymap(dpy, down.as_mut_ptr());
        if codes
            .iter()
            .any(|code| *code >= 256 || down[*code as usize / 8] & (1 << (*code % 8)) != 0)
        {
            return Err("release held keys before native input".into());
        }
        let mut pointer = 0;
        if client_pointer(dpy, 0, &mut pointer) == 0 {
            return Err("XInput client pointer is unavailable".into());
        }
        let device = if matches!(input, Input::Key(_)) {
            let mut count = 0;
            let devices = query(dpy, 1, &mut count);
            if devices.is_null() {
                return Err("XInput devices are unavailable".into());
            }
            let keyboard = std::slice::from_raw_parts(devices, count.max(0) as usize)
                .iter()
                .find(|device| {
                    device.usage == 2 && device.attachment == pointer && device.enabled != 0
                })
                .map(|device| device.id);
            free(devices);
            keyboard.ok_or("XInput keyboard is unavailable")?
        } else {
            pointer
        };
        if matches!(input, Input::Key(_)) && !view.grab_focus() {
            return Err("browser could not acquire keyboard focus".into());
        }
        let window = get_window(surface.to_glib_none().0);
        grab_server(dpy);
        let mut guard = Grab {
            display: dpy,
            device: None,
            pressed: Vec::new(),
            button_down: false,
            fake_button,
            fake,
            ungrab,
            release_server,
            flush,
            lock_modifiers,
            saved_caps_lock: None,
        };
        let mut bits = [
            if matches!(input, Input::Key(_)) {
                12u8
            } else {
                112u8
            },
            0,
            0,
            0,
        ]; // Keys, or pointer buttons and motion.
        let mut mask = EventMask {
            device,
            length: bits.len() as c_int,
            mask: bits.as_mut_ptr(),
        };
        if grab_device(dpy, device, window, 0, 0, 1, 1, 0, &mut mask) != 0 {
            return Err("input device is busy or the browser window is not viewable".into());
        }
        guard.device = Some(device);
        if matches!(input, Input::Key(_)) {
            if state(dpy, 0x100, &mut keyboard_state) != 0 {
                return Err("could not read the keyboard lock state".into());
            }
            if keyboard_state.locked_mods & 2 != 0 {
                // Keep logical key/Shift semantics; restore Caps Lock before releasing the grab.
                guard.saved_caps_lock = Some(keyboard_state.locked_mods);
                if lock_modifiers(dpy, 0x100, 2, 0) == 0 {
                    return Err("could not preserve native key semantics with Caps Lock".into());
                }
            }
        }
        // A grab can deliver events to an obscured window without activating CSS hover.
        raise(dpy, window);
        focus(dpy, window, 2, 0);
        if let Input::Pointer(action, target) = input {
            let to_root = |target: &PointerTarget| -> Result<(c_int, c_int), String> {
                let (x, y) = target.scale(f64::from(view.width()), f64::from(view.height()))?;
                let native = view.native().ok_or("browser has no native widget")?;
                let point = view
                    .compute_point(
                        native.upcast_ref::<gtk::Widget>(),
                        &gtk::graphene::Point::new(x as f32, y as f32),
                    )
                    .ok_or("could not map browser pointer coordinates")?;
                if !native
                    .upcast_ref::<gtk::Widget>()
                    .pick(
                        f64::from(point.x()),
                        f64::from(point.y()),
                        gtk::PickFlags::DEFAULT,
                    )
                    .is_some_and(|hit| {
                        hit == *view.upcast_ref::<gtk::Widget>() || hit.is_ancestor(view)
                    })
                {
                    return Err("browser pointer target is covered by a native widget".into());
                }
                let scale = f64::from(surface.scale_factor());
                let (surface_x, surface_y) = native.surface_transform();
                let (mut root_x, mut root_y, mut child) = (0, 0, 0);
                if translate(
                    dpy,
                    window,
                    root_window(dpy),
                    ((f64::from(point.x()) + surface_x) * scale).round() as c_int,
                    ((f64::from(point.y()) + surface_y) * scale).round() as c_int,
                    &mut root_x,
                    &mut root_y,
                    &mut child,
                ) == 0
                {
                    return Err("could not locate the browser window".into());
                }
                Ok((root_x, root_y))
            };
            let (root_x, root_y) = to_root(target)?;
            let drag_points = if let PointerAction::Drag { to_x, to_y } = action {
                (1..=16)
                    .map(|step| {
                        let fraction = f64::from(step) / 16.0;
                        to_root(&PointerTarget {
                            x: target.x + (to_x - target.x) * fraction,
                            y: target.y + (to_y - target.y) * fraction,
                            width: target.width,
                            height: target.height,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            if motion(dpy, -1, root_x, root_y, 0) == 0 {
                return Err("native pointer motion failed".into());
            }
            if !drag_points.is_empty() {
                if fake_button(dpy, 1, 1, 0) == 0 {
                    return Err("native pointer press failed".into());
                }
                guard.button_down = true;
                for (x, y) in drag_points {
                    if motion(dpy, -1, x, y, 0) == 0 {
                        return Err("native drag motion failed".into());
                    }
                }
                // The guard releases the held button before ungrabbing the device.
                return Ok(());
            }
            let count = match action {
                PointerAction::Hover => 0,
                PointerAction::Click => 1,
                PointerAction::DoubleClick => 2,
                PointerAction::Drag { .. } => unreachable!(),
            };
            for _ in 0..count {
                if fake_button(dpy, 1, 1, 0) == 0 {
                    return Err("native pointer press failed".into());
                }
                guard.button_down = true;
                if fake_button(dpy, 1, 0, 0) == 0 {
                    return Err("native pointer release failed".into());
                }
                guard.button_down = false;
            }
            return Ok(());
        }
        for code in codes {
            if fake(dpy, code, 1, 0) == 0 {
                return Err("native key injection failed".into());
            }
            guard.pressed.push(code);
        }
        Ok(())
    }
}
