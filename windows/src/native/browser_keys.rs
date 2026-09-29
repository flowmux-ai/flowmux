// SPDX-License-Identifier: GPL-3.0-or-later
//! Native accelerator authority, with a read-only DOM composition guard.
use super::*;
use crate::keybindings::{Binding, Chord};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2Frame, ICoreWebView2Frame2, ICoreWebView2Frame7, ICoreWebView2_4,
    COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
};
use windows::core::Interface;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState;

#[derive(Default)]
pub(super) struct Keys {
    pub revision: Uuid,
    pub enabled: bool,
    pub focus_epoch: u64,
    bindings: Vec<Binding>,
    frames: HashMap<Uuid, (ICoreWebView2Frame2, bool)>,
    frames_complete: bool,
    untracked_navigations: HashSet<u64>,
}
impl Keys {
    pub fn lost_focus(&mut self) {
        self.focus_epoch = self.focus_epoch.wrapping_add(1);
    }
    pub fn sync(&mut self, enabled: bool, settings: &crate::settings::Document) {
        if self.revision != settings.revision || self.bindings.is_empty() {
            self.bindings = crate::keybindings::resolved(&settings.keybindings).unwrap_or_default();
            self.revision = settings.revision;
        }
        if self.enabled && !enabled {
            self.focus_epoch = self.focus_epoch.wrapping_add(1);
        }
        self.enabled = enabled;
    }
    pub fn binding(&self, chord: &Chord) -> Option<Binding> {
        self.ready()
            .then(|| self.bindings.iter().find(|b| &b.chord == chord).cloned())
            .flatten()
    }
    pub fn ready(&self) -> bool {
        self.enabled
            && self.frames_complete
            && self.untracked_navigations.is_empty()
            && self.frames.len() <= 128
    }
    pub fn frames(&self) -> Option<Vec<(ICoreWebView2Frame2, bool)>> {
        // ponytail: query at most 128 frames per chord; native focused-frame
        // selection would remove this ceiling if large frame trees need it.
        (self.frames_complete && self.frames.len() <= 128)
            .then(|| self.frames.values().cloned().collect())
    }
    pub fn nested_tracking(&self) -> bool {
        self.frames_complete && self.frames.values().all(|(_, nested)| *nested)
    }
}

fn track_frame(state: &Rc<RefCell<Keys>>, frame: ICoreWebView2Frame) {
    let id = Uuid::new_v4();
    let install = || -> windows::core::Result<()> {
        let frame: ICoreWebView2Frame2 = frame.cast()?;
        let mut nested_tracking = false;
        unsafe {
            let weak = Rc::downgrade(state);
            frame.add_Destroyed(
                &webview2_com::FrameDestroyedEventHandler::create(Box::new(move |_, _| {
                    if let Some(state) = weak.upgrade() {
                        let removed = {
                            let mut state = state.borrow_mut();
                            state.lost_focus();
                            let removed = state.frames.remove(&id);
                            if state.frames.is_empty() {
                                state.untracked_navigations.clear();
                            }
                            removed
                        };
                        drop(removed);
                    }
                    Ok(())
                })),
                &mut 0,
            )?;
            let weak = Rc::downgrade(state);
            frame.add_NavigationStarting(
                &webview2_com::FrameNavigationStartingEventHandler::create(Box::new(
                    move |_, args| {
                        if let (Some(state), Some(args)) = (weak.upgrade(), args) {
                            let mut navigation = 0;
                            args.NavigationId(&mut navigation)?;
                            let mut state = state.borrow_mut();
                            state.untracked_navigations.remove(&navigation);
                            state.lost_focus();
                        }
                        Ok(())
                    },
                )),
                &mut 0,
            )?;
            if let Ok(nested) = frame.cast::<ICoreWebView2Frame7>() {
                let weak = Rc::downgrade(state);
                nested.add_FrameCreated(
                    &webview2_com::FrameChildFrameCreatedEventHandler::create(Box::new(
                        move |_, args| {
                            if let (Some(state), Some(args)) = (weak.upgrade(), args) {
                                match args.Frame() {
                                    Ok(frame) => track_frame(&state, frame),
                                    Err(_) => {
                                        let mut state = state.borrow_mut();
                                        state.frames_complete = false;
                                        state.lost_focus();
                                    }
                                }
                            }
                            Ok(())
                        },
                    )),
                    &mut 0,
                )?;
                nested_tracking = true;
            }
        }
        state
            .borrow_mut()
            .frames
            .insert(id, (frame, nested_tracking));
        Ok(())
    };
    let complete = install().is_ok();
    let mut state = state.borrow_mut();
    state.frames_complete &= complete;
    state.lost_focus();
}

pub(super) fn install(browser: &Browser, id: SurfaceId) -> anyhow::Result<()> {
    let state = browser.keys.clone();
    let holder = browser.holder.window as isize;
    let instance = browser.instance;
    let background = browser.background;
    unsafe {
        // The core event also covers descendants omitted by old Frame APIs.
        // Their NavigationStarting callbacks remove known navigation IDs below.
        let weak = Rc::downgrade(&state);
        browser
            .view
            .controller()
            .CoreWebView2()?
            .add_FrameNavigationStarting(
                &webview2_com::NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    if let (Some(state), Some(args)) = (weak.upgrade(), args) {
                        let mut navigation = 0;
                        args.NavigationId(&mut navigation)?;
                        let mut state = state.borrow_mut();
                        if state.untracked_navigations.len() < 128 {
                            state.untracked_navigations.insert(navigation);
                        } else {
                            state.frames_complete = false;
                        }
                        state.lost_focus();
                    }
                    Ok(())
                })),
                &mut 0,
            )?;
        if let Ok(core) = browser
            .view
            .controller()
            .CoreWebView2()?
            .cast::<ICoreWebView2_4>()
        {
            let weak = Rc::downgrade(&state);
            let complete = core
                .add_FrameCreated(
                    &webview2_com::FrameCreatedEventHandler::create(Box::new(move |_, args| {
                        if let (Some(state), Some(args)) = (weak.upgrade(), args) {
                            match args.Frame() {
                                Ok(frame) => track_frame(&state, frame),
                                Err(_) => {
                                    let mut state = state.borrow_mut();
                                    state.frames_complete = false;
                                    state.lost_focus();
                                }
                            }
                        }
                        Ok(())
                    })),
                    &mut 0,
                )
                .is_ok();
            state.borrow_mut().frames_complete = complete;
        }
        browser.view.controller().add_AcceleratorKeyPressed(
            &webview2_com::AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else {
                    return Ok(());
                };
                // Hidden verifiers exercise dispatch through the owned debug IPC;
                // never inspect the user's live keyboard state in that mode.
                if background {
                    return Ok(());
                }
                let root = GetAncestor(holder as HWND, GA_ROOT);
                let mut handled = Default::default();
                args.Handled(&mut handled)?;
                if handled.as_bool()
                    || root.is_null()
                    || GetCapture() == root
                    || GetForegroundWindow() != root
                    || IsWindowEnabled(root) == 0
                    || IsWindowVisible(holder as HWND) == 0
                {
                    return Ok(());
                }
                let mut kind = Default::default();
                let mut key = 0;
                let mut flags = 0;
                args.KeyEventKind(&mut kind)?;
                args.VirtualKey(&mut key)?;
                args.KeyEventLParam(&mut flags)?;
                if !matches!(
                    kind,
                    COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                        | COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
                ) || [0xa5, 0x5b, 0x5c].into_iter().any(|k| GetKeyState(k) < 0)
                {
                    return Ok(());
                }
                let chord = crate::keybindings::captured_key(
                    key,
                    GetKeyState(0x11) < 0,
                    GetKeyState(0x12) < 0,
                    GetKeyState(0x10) < 0,
                )
                .ok()
                .flatten()
                .and_then(|s| crate::keybindings::parse(&s).ok());
                let selected = {
                    let state = state.borrow();
                    chord
                        .and_then(|c| state.binding(&c))
                        .map(|binding| (state.revision, state.focus_epoch, binding))
                };
                let Some((revision, focus_epoch, binding)) = selected else {
                    return Ok(());
                };
                // COM can deliver LostFocus reentrantly; release the state borrow first.
                args.SetHandled(true)?;
                if flags as u32 & (1 << 30) == 0 {
                    post(Event::Browser(Signal::PageShortcut(
                        id,
                        instance,
                        revision,
                        focus_epoch,
                        binding,
                    )));
                }
                Ok(())
            })),
            &mut 0,
        )?;
        let state = browser.keys.clone();
        browser.view.controller().add_LostFocus(
            &webview2_com::FocusChangedEventHandler::create(Box::new(move |_, _| {
                let mut state = state.borrow_mut();
                state.lost_focus();
                Ok(())
            })),
            &mut 0,
        )?;
    }
    Ok(())
}

pub(super) fn script(key: &str, background: bool) -> String {
    // No WebMessage/host object bridge. Page script can read this immutable
    // guard, but only a native accelerator can authorize an application action.
    // Native frame tracking supplies the scope; missing descendants stay blocked.
    format!(
        r#"(()=>{{
      const key={key:?}, test={background};
      const doc=document,apply=Reflect.apply;
      const rootGet=Object.getOwnPropertyDescriptor(Document.prototype,'documentElement').get;
      const hasFocus=Document.prototype.hasFocus;
      const lengthGet=Object.getOwnPropertyDescriptor(window,'length')?.get;
      const keyGet=Object.getOwnPropertyDescriptor(KeyboardEvent.prototype,'key').get;
      const codeGet=Object.getOwnPropertyDescriptor(KeyboardEvent.prototype,'keyCode').get;
      const composingGet=Object.getOwnPropertyDescriptor(KeyboardEvent.prototype,'isComposing').get;
      let composing=false,settling=false;
      let root=apply(rootGet,doc,[]);
      const listen=window.addEventListener.bind(window);
      listen('DOMContentLoaded',e=>{{if(e.isTrusted||test)root=apply(rootGet,doc,[]);}},{{once:true,capture:true}});
      listen('compositionstart',e=>{{if(e.isTrusted||test){{composing=true;settling=true;}}}},true);
      listen('compositionend',e=>{{if(e.isTrusted||test){{composing=false;settling=true;}}}},true);
      listen('blur',e=>{{if((e.isTrusted||test)&&e.target===window){{composing=false;settling=false;}}}},true);
      listen('keydown',e=>{{if(!e.isTrusted&&!test)return;try{{if(apply(composingGet,e,[])||apply(codeGet,e,[])===229||apply(keyGet,e,[])==='Process'||apply(keyGet,e,[])==='Dead')settling=true;}}catch{{}}}},true);
      listen('keyup',e=>{{if(!e.isTrusted&&!test)return;try{{const k=apply(keyGet,e,[]);if(k!=='Shift'&&k!=='Control'&&k!=='Alt'&&k!=='Meta')settling=false;}}catch{{}}}},true);
      Object.defineProperty(window,key,{{value:(mode=0)=>{{
        if(mode&&!test&&!apply(hasFocus,doc,[]))return true;
        // document.open can remove the composition listeners while retaining window.
        if(!root||root!==apply(rootGet,doc,[])||composing||settling)return false;
        // Older runtimes cannot enumerate descendants, including closed shadow trees.
        if(mode===1)return true;
        return !!lengthGet&&apply(lengthGet,window,[])===0;
      }}}});
    }})()"#
    )
}
