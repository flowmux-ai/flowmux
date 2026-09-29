// SPDX-License-Identifier: GPL-3.0-or-later
//! Native accelerator authority, with a read-only DOM composition guard.
use super::*;
use crate::keybindings::{Binding, Chord};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState;

#[derive(Default)]
pub(super) struct Keys {
    pub revision: Uuid,
    pub enabled: bool,
    pub focus_epoch: u64,
    bindings: Vec<Binding>,
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
        self.enabled
            .then(|| self.bindings.iter().find(|b| &b.chord == chord).cloned())
            .flatten()
    }
}

pub(super) fn install(browser: &Browser, id: SurfaceId) -> anyhow::Result<()> {
    let state = browser.keys.clone();
    let holder = browser.holder.window as isize;
    let instance = browser.instance;
    let background = browser.background;
    unsafe {
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
    // ponytail: inaccessible frames cancel shortcuts; use native frame queries
    // when cross-origin iframe shortcuts are supported.
    format!(
        r#"(()=>{{
      const key={key:?}, test={background};
      const doc=document,apply=Reflect.apply;
      const activeGet=Object.getOwnPropertyDescriptor(Document.prototype,'activeElement').get;
      const rootGet=Object.getOwnPropertyDescriptor(Document.prototype,'documentElement').get;
      const tagGet=Object.getOwnPropertyDescriptor(Element.prototype,'tagName').get;
      const frameGet=Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype,'contentWindow').get;
      const legacyFrameGet=Object.getOwnPropertyDescriptor(HTMLFrameElement.prototype,'contentWindow').get;
      const keyGet=Object.getOwnPropertyDescriptor(KeyboardEvent.prototype,'key').get;
      const codeGet=Object.getOwnPropertyDescriptor(KeyboardEvent.prototype,'keyCode').get;
      const composingGet=Object.getOwnPropertyDescriptor(KeyboardEvent.prototype,'isComposing').get;
      let composing=false,settling=false;
      let root=apply(rootGet,doc,[]);
      const listen=window.addEventListener.bind(window);
      listen('DOMContentLoaded',e=>{{if(e.isTrusted||test)root=apply(rootGet,doc,[]);}},{{once:true,capture:true}});
      listen('compositionstart',e=>{{if(e.isTrusted||test){{composing=true;settling=true;}}}},true);
      listen('compositionend',e=>{{if(e.isTrusted||test){{composing=false;settling=true;}}}},true);
      listen('keydown',e=>{{if(!e.isTrusted&&!test)return;try{{if(apply(composingGet,e,[])||apply(codeGet,e,[])===229||apply(keyGet,e,[])==='Process'||apply(keyGet,e,[])==='Dead')settling=true;}}catch{{}}}},true);
      listen('keyup',e=>{{if(!e.isTrusted&&!test)return;try{{const k=apply(keyGet,e,[]);if(k!=='Shift'&&k!=='Control'&&k!=='Alt'&&k!=='Meta')settling=false;}}catch{{}}}},true);
      Object.defineProperty(window,key,{{value:()=>{{
        // document.open can remove the composition listeners while retaining window.
        if(!root||root!==apply(rootGet,doc,[])||composing||settling)return false;
        const active=apply(activeGet,doc,[]),tag=active&&apply(tagGet,active,[]);
        if(tag==='IFRAME'||tag==='FRAME'){{
          try{{return apply(tag==='IFRAME'?frameGet:legacyFrameGet,active,[])[key]()===true;}}catch{{return false;}}
        }}
        return true;
      }}}});
    }})()"#
    )
}
