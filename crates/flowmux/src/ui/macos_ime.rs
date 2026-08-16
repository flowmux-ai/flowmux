// SPDX-License-Identifier: GPL-3.0-or-later
//! macOS IME repair for GTK's quartz input path.
//!
//! GTK 4's macOS backend (`GdkMacosBaseView`, `gdk/macos/GdkMacosBaseView.c`)
//! implements `NSTextInputClient` by staging what the input method hands it on
//! the `GdkSurface` and letting the quartz immodule read it back after the key
//! was fed to the view: `insertText:replacementRange:` stores the string under
//! the `"tic-insert-text"` data key with `g_object_set_data_full`, i.e. **one
//! slot, overwritten on every call**. The macOS Korean IME (and other IMEs
//! whose composition is terminated implicitly by the next key) calls
//! `insertText:` twice inside a single `keyDown:` when a composing syllable is
//! followed by a non-jamo key: once with the finished syllable, once with the
//! key's own character. The second call overwrites the first, so "안녕," reaches
//! the terminal as "안,", "아1" as "1", "안녕하세요!" as "안녕하세!".
//!
//! This is the one place the text is lost — Enter, arrows and the delete
//! selectors already round-trip correctly — so fix it there instead of guessing
//! per key at the widget layer: swap the method's implementation for one that
//! *appends* to whatever is already staged for the current key event and then
//! calls the original. The quartz immodule nulls the slot when it consumes it
//! (`output_result`), so the accumulator can never carry text across events.
//!
//! Everything here is private GTK API by nature; if the class or method is not
//! found, nothing is installed and behaviour is exactly upstream GTK.

use std::ffi::{c_char, c_void, CStr};
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{msg_send, sel};
use objc2_foundation::{NSRange, NSString};

/// `-[GdkMacosBaseView insertText:replacementRange:]`
type InsertTextImp = unsafe extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, NSRange);

static ORIGINAL_INSERT_TEXT: OnceLock<InsertTextImp> = OnceLock::new();

/// Data key GTK stages the pending insert string under (`TIC_INSERT_TEXT`).
const TIC_INSERT_TEXT: &CStr = c"tic-insert-text";

/// Install the accumulating `insertText:` override. Idempotent; safe to call
/// before GTK is initialised (the class is registered when libgtk loads).
pub fn install_insert_text_accumulation() {
    if ORIGINAL_INSERT_TEXT.get().is_some() {
        return;
    }
    let Some(class) = AnyClass::get(c"GdkMacosBaseView") else {
        tracing::warn!("GdkMacosBaseView not found; leaving quartz IME insert-text path as is");
        return;
    };
    let sel = sel!(insertText:replacementRange:);
    let Some(method) = class.instance_method(sel) else {
        tracing::warn!("GdkMacosBaseView lacks insertText:replacementRange:; not patching");
        return;
    };
    // SAFETY: the method's real signature is `void (id, SEL, id, NSRange)`;
    // `InsertTextImp` matches it exactly, and our replacement forwards every
    // call to the original with the same argument shape.
    unsafe {
        let original = std::mem::transmute::<Imp, InsertTextImp>(method.implementation());
        if ORIGINAL_INSERT_TEXT.set(original).is_err() {
            return;
        }
        let replacement: InsertTextImp = insert_text_accumulating;
        method.set_implementation(std::mem::transmute::<InsertTextImp, Imp>(replacement));
    }
    tracing::debug!("installed accumulating insertText: on GdkMacosBaseView");
}

/// Text already staged on the view's `GdkSurface` for this key event, if any.
unsafe fn pending_insert_text(view: &AnyObject) -> Option<String> {
    let surface: *mut c_void = msg_send![view, gdkSurface];
    if surface.is_null() {
        return None;
    }
    let raw = gtk::glib::gobject_ffi::g_object_get_data(surface.cast(), TIC_INSERT_TEXT.as_ptr())
        as *const c_char;
    if raw.is_null() {
        return None;
    }
    let text = CStr::from_ptr(raw).to_str().ok()?;
    (!text.is_empty()).then(|| text.to_owned())
}

/// `aString` is documented as either `NSString` or `NSAttributedString`.
unsafe fn plain_string(string: &AnyObject) -> Retained<NSString> {
    let is_attributed = AnyClass::get(c"NSAttributedString")
        .is_some_and(|cls| msg_send![string, isKindOfClass: cls]);
    if is_attributed {
        msg_send![string, string]
    } else {
        Retained::retain(string as *const AnyObject as *mut NSString)
            .expect("insertText: string is non-null")
    }
}

unsafe extern "C-unwind" fn insert_text_accumulating(
    this: &AnyObject,
    cmd: Sel,
    string: &AnyObject,
    range: NSRange,
) {
    let original = ORIGINAL_INSERT_TEXT
        .get()
        .expect("override installed after original was recorded");
    match pending_insert_text(this) {
        None => original(this, cmd, string, range),
        Some(pending) => {
            let combined = format!("{pending}{}", plain_string(string));
            original(this, cmd, &NSString::from_str(&combined), range);
        }
    }
}
