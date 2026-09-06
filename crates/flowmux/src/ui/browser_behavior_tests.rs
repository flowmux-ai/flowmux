// SPDX-License-Identifier: GPL-3.0-or-later
//! Execute the shared action scripts in the production WebKit pane. A syntax
//! error, missing event, or wrong DOM effect must fail, even if source text
//! still contains the expected method names. No network or persistent profile.

use super::*;
use flowmux_browser::{scripts::*, DomSnapshot};
use gtk::glib;
use std::time::{Duration, Instant};

struct Page {
    pane: BrowserPane,
    window: gtk::Window,
}

impl Page {
    async fn new(html: &str) -> Self {
        let pane = BrowserPane::new(
            PaneId::new(),
            SurfaceId::new(),
            None,
            PaneCallbacks::noop_for_test(),
            BrowserEngine::Webkit,
            false,
        );
        let window = gtk::Window::builder()
            .default_width(800)
            .default_height(600)
            .child(&pane.root)
            .build();
        window.present();
        pane.web_view.load_html(
            &format!("<!doctype html><title>flowmux test</title>{html}"),
            Some("file:///flowmux-test/"),
        );
        let page = Self { pane, window };
        page.wait("document.title === 'flowmux test' && document.readyState === 'complete' && innerWidth > 0").await;
        page
    }

    async fn eval(&self, script: &str) -> String {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pane.evaluate_js(script, move |result| {
            let _ = tx.send(result);
        });
        glib::future_with_timeout(Duration::from_secs(10), rx)
            .await
            .expect("WebKit evaluation timed out")
            .expect("WebKit dropped evaluation callback")
            .unwrap_or_else(|error| panic!("{error}\nscript: {script}"))
    }

    async fn wait(&self, predicate: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.eval(predicate).await != "true" {
            assert!(Instant::now() < deadline, "page did not reach: {predicate}");
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    }

    async fn ok(&self, script: String) {
        assert_eq!(self.eval(&script).await, "ok", "script: {script}");
    }

    async fn events(&self) -> Vec<String> {
        serde_json::from_str(&self.eval("JSON.stringify(events.splice(0))").await).unwrap()
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        self.pane.prepare_for_close();
        self.window.destroy();
    }
}

#[gtk::test]
async fn snapshot_resolves_real_elements_without_mutating_the_document() {
    let page = Page::new(
        r#"
        <h1>Heading</h1><button id='odd:id' aria-label='Say "hello"'>Button</button>
        <div><button id='duplicate'>First</button><button id='duplicate'>Second</button></div>
        <input placeholder='Your name'><a href='/target'>Link</a>
        <button hidden>Hidden</button><button style='opacity:0'>Transparent</button>
    "#,
    )
    .await;
    page.eval("window.mutations = []; new MutationObserver(changes => mutations.push(...changes)).observe(document.documentElement, {subtree:true, childList:true, attributes:true, characterData:true})").await;
    let before = page.eval("document.documentElement.outerHTML").await;
    let snapshot: DomSnapshot = serde_json::from_str(&page.eval(SNAPSHOT_JS).await).unwrap();
    assert_eq!(snapshot.page.title, "flowmux test");
    assert_eq!(snapshot.page.ready_state, "complete");
    assert_eq!(snapshot.refs.len(), 6);
    for name in [
        "Heading",
        "Say \"hello\"",
        "First",
        "Second",
        "Your name",
        "Link",
    ] {
        assert!(
            snapshot.refs.values().any(|r| r.name == name),
            "missing {name}"
        );
    }
    assert!(snapshot.markdown.contains(r#"Say \"hello\""#));
    for (token, meta) in &snapshot.refs {
        assert!(snapshot.markdown.contains(&format!("[ref={token}]")));
        assert_eq!(page.eval(&count_selector(&meta.selector)).await, "1");
    }
    assert_eq!(
        page.eval("document.documentElement.outerHTML").await,
        before
    );
    assert_eq!(page.eval("mutations.length").await, "0");

    // Use a snapshot selector to act on the intended duplicate-ID button.
    let second = snapshot.refs.values().find(|r| r.name == "Second").unwrap();
    page.eval("window.clicked = ''; document.addEventListener('click', e => clicked = e.target.textContent)").await;
    page.ok(click_by_selector(&second.selector)).await;
    assert_eq!(page.eval("clicked").await, "Second");
}

#[gtk::test]
async fn form_actions_preserve_text_and_deliver_events_to_page_listeners() {
    let page = Page::new(r#"
        <textarea id='text'></textarea><input id='input'>
        <select id='select'><option value='one'>First</option><option value='two'>Second</option></select>
        <input id='check' type='checkbox'>
        <input id='radio1' type='radio' name='group' checked><input id='radio2' type='radio' name='group'>
        <script>
          window.events = [];
          for (const name of ['input','change','keydown','keyup'])
            document.addEventListener(name, e => events.push(e.target.id + ':' + name + (e.key ? ':' + e.key : '')));
        </script>
    "#).await;
    let text = "한글 😀 O'Reilly \"quoted\" \\ end\n\t\u{0001}\u{2028}\u{2029}\"); window.injected = true; //";
    page.ok(fill_by_selector("#text", text)).await;
    assert_eq!(page.eval(&value_of_selector("#text")).await, text);
    assert_eq!(page.eval("typeof injected").await, "undefined");
    assert_eq!(page.events().await, ["text:input", "text:change"]);

    page.ok(focus_by_selector("#input")).await;
    assert_eq!(page.eval("document.activeElement.id").await, "input");
    page.ok(type_keys("가😀")).await;
    assert_eq!(page.eval(&value_of_selector("#input")).await, "가😀");
    assert_eq!(
        page.events().await,
        [
            "input:keydown:가",
            "input:input",
            "input:keyup:가",
            "input:keydown:😀",
            "input:input",
            "input:keyup:😀",
            "input:change"
        ]
    );
    page.ok(press_key("Enter")).await;
    assert_eq!(
        page.events().await,
        ["input:keydown:Enter", "input:keyup:Enter"]
    );
    page.ok(blur_by_selector("#input")).await;
    assert_ne!(page.eval("document.activeElement.id").await, "input");

    for (value, expected) in [("two", "two"), ("First", "one")] {
        page.ok(select_option_by_selector("#select", value)).await;
        assert_eq!(page.eval(&value_of_selector("#select")).await, expected);
        assert_eq!(page.events().await, ["select:change"]);
    }
    assert_eq!(
        page.eval(&select_option_by_selector("#select", "missing"))
            .await,
        "error: option not found"
    );
    assert_eq!(page.eval(&value_of_selector("#select")).await, "one");
    assert!(page.events().await.is_empty());

    for checked in [true, false] {
        for _ in 0..2 {
            page.ok(if checked {
                check_by_selector("#check")
            } else {
                uncheck_by_selector("#check")
            })
            .await;
        }
        assert_eq!(
            page.eval(&is_checked_selector("#check")).await,
            checked.to_string()
        );
        assert_eq!(
            page.events().await,
            ["check:change"],
            "repeat action must be idempotent"
        );
    }
    page.ok(check_by_selector("#radio2")).await;
    assert_eq!(page.eval(&is_checked_selector("#radio1")).await, "false");
    assert_eq!(page.eval(&is_checked_selector("#radio2")).await, "true");
    assert_eq!(page.events().await, ["radio2:change"]);
    assert_eq!(
        page.eval(&uncheck_by_selector("#radio2")).await,
        "error: not a checkbox"
    );
    assert_eq!(
        page.eval(&check_by_selector("#input")).await,
        "error: not checkable"
    );
    assert_eq!(page.eval(&is_checked_selector("#radio2")).await, "true");
    assert!(page.events().await.is_empty());
}

#[gtk::test]
async fn pointer_actions_queries_and_missing_targets_use_the_real_dom() {
    let page = Page::new(
        r#"
        <button id='button' title='한글'>Go</button><button id='disabled' disabled>Disabled</button>
        <div id='hidden' style='display:none'>Hidden</div>
        <div id='invisible' style='visibility:hidden'>Invisible</div>
        <div id='transparent' style='opacity:0'>Transparent</div>
        <div id='zero' style='width:0;height:0;overflow:hidden'>Zero</div>
        <div style='height:2000px'></div><button id='bottom'>Bottom</button>
        <script>
          window.events = [];
          for (const name of ['click','dblclick','mouseenter','mouseover'])
            document.querySelector('#button').addEventListener(name, e => events.push(name));
        </script>
    "#,
    )
    .await;
    page.ok(click_by_selector("#button")).await;
    page.ok(dblclick_by_selector("#button")).await;
    page.ok(hover_by_selector("#button")).await;
    assert_eq!(
        page.events().await,
        ["click", "dblclick", "mouseenter", "mouseover"]
    );
    assert_eq!(page.eval(&text_of_selector("#button")).await, "Go");
    assert_eq!(
        page.eval(&attr_of_selector("#button", "title")).await,
        "한글"
    );
    assert_eq!(page.eval(&attr_of_selector("#button", "absent")).await, "");
    assert_eq!(page.eval(&count_selector("button")).await, "3");
    assert_eq!(page.eval(&count_selector(".missing")).await, "0");
    assert_eq!(page.eval(&is_enabled_selector("#button")).await, "true");
    assert_eq!(page.eval(&is_enabled_selector("#disabled")).await, "false");
    assert_eq!(page.eval(&is_visible_selector("#button")).await, "true");
    for id in ["hidden", "invisible", "transparent", "zero"] {
        assert_eq!(
            page.eval(&is_visible_selector(&format!("#{id}"))).await,
            "false",
            "{id}"
        );
    }
    page.ok(scroll_by_selector("#bottom", 0, 0)).await;
    page.wait("scrollY > 0 && document.querySelector('#bottom').getBoundingClientRect().top < innerHeight").await;

    for script in [
        click_by_selector("#missing"),
        dblclick_by_selector("#missing"),
        hover_by_selector("#missing"),
        focus_by_selector("#missing"),
        blur_by_selector("#missing"),
        fill_by_selector("#missing", "x"),
        select_option_by_selector("#missing", "x"),
        scroll_by_selector("#missing", 0, 0),
        text_of_selector("#missing"),
        value_of_selector("#missing"),
        attr_of_selector("#missing", "x"),
        check_by_selector("#missing"),
        uncheck_by_selector("#missing"),
        is_visible_selector("#missing"),
        is_enabled_selector("#missing"),
        is_checked_selector("#missing"),
    ] {
        assert_eq!(page.eval(&script).await, "error: not found", "{script}");
    }
}

#[gtk::test]
async fn navigation_tracks_history_reload_and_invalidates_snapshot_refs() {
    let page = Page::new("<button>Start</button>").await;
    let directory = tempfile::tempdir().unwrap();
    let paths: Vec<_> = ["first", "second", "third"]
        .into_iter()
        .map(|name| {
            let path = directory.path().join(format!("{name}.html"));
            std::fs::write(
                &path,
                format!("<!doctype html><title>{name}</title><button>{name}</button>"),
            )
            .unwrap();
            gtk::gio::File::for_path(path).uri().to_string()
        })
        .collect();
    page.pane.load_uri(&paths[0]);
    page.wait("document.title === 'first' && document.readyState === 'complete'")
        .await;
    assert_eq!(page.pane.current_url(), paths[0]);
    let snapshot: DomSnapshot = serde_json::from_str(&page.eval(SNAPSHOT_JS).await).unwrap();
    page.pane
        .refs
        .borrow_mut()
        .populate_from_snapshot(page.pane.ref_scope, &snapshot);
    assert!(!page.pane.refs.borrow().is_empty(page.pane.ref_scope));
    page.pane.load_uri(&paths[1]);
    page.wait("document.title === 'second' && document.readyState === 'complete'")
        .await;
    assert!(page.pane.refs.borrow().is_empty(page.pane.ref_scope));
    assert_eq!(page.pane.current_title(), "second");
    assert!(!page.pane.go_forward());
    assert!(page.pane.go_back());
    page.wait("document.title === 'first'").await;
    assert!(page.pane.go_forward());
    page.wait("document.title === 'second'").await;
    page.eval("document.querySelector('button').textContent = 'modified'")
        .await;
    page.pane.reload();
    page.wait("document.querySelector('button')?.textContent === 'second'")
        .await;
    assert!(page.pane.go_back());
    page.wait("document.title === 'first'").await;
    page.pane.load_uri(&paths[2]);
    page.wait("document.title === 'third'").await;
    assert!(
        !page.pane.go_forward(),
        "navigating after back must discard forward history"
    );
}
