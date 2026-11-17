// SPDX-License-Identifier: GPL-3.0-or-later
//! Process-local mitigation for GTK 4.14.5's icon-cache finalization race.
//! GtkImage requests preloads internally, so changing application lookups is insufficient.

use gtk::{gio::ffi as gio, glib::gobject_ffi as gobject};
use std::sync::OnceLock;

type RunInThread = unsafe extern "C" fn(*mut gio::GTask, gio::GTaskThreadFunc);

fn affected_version(version: (u32, u32, u32)) -> bool {
    version == (4, 14, 5)
}

/// Interpose only this executable's GIO calls; no preload library or system change.
///
/// Safety: GIO callers supply a live GTask and its matching task function.
#[no_mangle]
unsafe extern "C" fn g_task_run_in_thread(task: *mut gio::GTask, task_func: gio::GTaskThreadFunc) {
    static AFFECTED: OnceLock<bool> = OnceLock::new();
    let affected = *AFFECTED.get_or_init(|| {
        affected_version((
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version(),
        ))
    });
    if affected && !task.is_null() {
        let source = gio::g_task_get_source_object(task);
        if !source.is_null()
            && gobject::g_type_check_instance_is_a(
                source.cast(),
                gtk::ffi::gtk_icon_paintable_get_type(),
            ) != 0
        {
            if let Some(run) = task_func {
                // GTK's load_icon_thread only loads the texture and returns the task result.
                // Finish it here so a worker cannot release the cached icon during lookup.
                // shortcut: cold icon decoding is synchronous on 4.14.5; remove when GTK's
                // weak-reference cache fix is available in the supported distribution runtime.
                run(
                    task,
                    source,
                    gio::g_task_get_task_data(task),
                    gio::g_task_get_cancellable(task),
                );
                return;
            }
        }
    }

    static ORIGINAL: OnceLock<RunInThread> = OnceLock::new();
    let run = ORIGINAL.get_or_init(|| {
        let symbol = libc::dlsym(libc::RTLD_NEXT, c"g_task_run_in_thread".as_ptr());
        if symbol.is_null() {
            eprintln!("Flowmux: could not resolve GIO g_task_run_in_thread");
            std::process::abort();
        }
        // RTLD_NEXT resolves the original GIO function with this public C ABI.
        std::mem::transmute::<*mut libc::c_void, RunInThread>(symbol)
    });
    run(task, task_func);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::prelude::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn only_verified_gtk_version_is_affected() {
        assert!(affected_version((4, 14, 5)));
        for version in [(4, 12, 5), (4, 14, 4), (4, 14, 6), (4, 16, 0), (4, 22, 3)] {
            assert!(!affected_version(version));
        }
    }

    #[gtk::test]
    fn gtk_internal_preloads_release_on_the_lookup_thread() {
        if !affected_version((
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version(),
        )) {
            return;
        }
        let theme = gtk::IconTheme::new();
        theme.set_theme_name(Some("Adwaita"));
        assert!(theme.has_icon("folder"));
        let owner = std::thread::current().id();
        let released = Arc::new(AtomicUsize::new(0));
        let wrong_thread = Arc::new(AtomicBool::new(false));
        let context = gtk::glib::MainContext::ref_thread_default();
        for _ in 0..1000 {
            // Above GTK's 128px LRU limit: no extra cache reference masks finalization.
            let icon = theme.lookup_icon(
                "folder",
                &[],
                129,
                1,
                gtk::TextDirection::None,
                gtk::IconLookupFlags::PRELOAD,
            );
            let released = Arc::clone(&released);
            let wrong_thread = Arc::clone(&wrong_thread);
            icon.add_weak_ref_notify(move || {
                if std::thread::current().id() != owner {
                    wrong_thread.store(true, Ordering::SeqCst);
                }
                released.fetch_add(1, Ordering::SeqCst);
            });
            drop(icon);
            while context.pending() {
                context.iteration(false);
            }
        }
        assert_eq!(released.load(Ordering::SeqCst), 1000);
        assert!(!wrong_thread.load(Ordering::SeqCst));
    }

    #[test]
    fn unrelated_gio_tasks_still_run_on_workers() {
        unsafe extern "C" fn work(
            task: *mut gio::GTask,
            _: *mut gobject::GObject,
            data: *mut libc::c_void,
            _: *mut gio::GCancellable,
        ) {
            let sender = &*data.cast::<std::sync::mpsc::Sender<std::thread::ThreadId>>();
            sender.send(std::thread::current().id()).unwrap();
            gio::g_task_return_boolean(task, 1);
        }
        unsafe extern "C" fn free_sender(data: *mut libc::c_void) {
            drop(Box::from_raw(
                data.cast::<std::sync::mpsc::Sender<std::thread::ThreadId>>(),
            ));
        }
        let source = gtk::gio::Cancellable::new();
        for source in [std::ptr::null_mut(), source.as_ptr().cast()] {
            let (sender, receiver) = std::sync::mpsc::channel::<std::thread::ThreadId>();
            unsafe {
                let task =
                    gio::g_task_new(source, std::ptr::null_mut(), None, std::ptr::null_mut());
                gio::g_task_set_task_data(
                    task,
                    Box::into_raw(Box::new(sender)).cast(),
                    Some(free_sender),
                );
                gio::g_task_run_in_thread(task, Some(work));
                gobject::g_object_unref(task.cast());
            }
            assert_ne!(
                receiver
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap(),
                std::thread::current().id()
            );
        }
    }
}
