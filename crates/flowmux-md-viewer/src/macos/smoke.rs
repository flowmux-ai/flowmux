// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use objc2_app_kit::NSAppearanceCustomization;

fn check_theme(viewer: &Viewer, bg: [u8; 3], fg: &str, appearance: &str) {
    let color = viewer.window.backgroundColor();
    let channel = |v: f64| (v * 255.0).round() as u8;
    assert_eq!(
        [
            channel(color.redComponent()),
            channel(color.greenComponent()),
            channel(color.blueComponent())
        ],
        bg,
        "window chrome must match the document"
    );
    assert_eq!(
        viewer.window.appearance().unwrap().name().to_string(),
        appearance
    );
    let [r, g, b] = bg;
    assert_eq!(
        viewer
            .javascript("return getComputedStyle(document.body).backgroundColor;")
            .unwrap(),
        format!("rgb({r}, {g}, {b})")
    );
    assert_eq!(
        viewer
            .javascript("return getComputedStyle(document.documentElement).color;")
            .unwrap(),
        fg
    );
}

fn click(viewer: &Viewer, tag: isize) {
    let root = viewer.window.contentView().unwrap();
    let button = root
        .subviews()
        .to_vec()
        .into_iter()
        .filter_map(|view| view.downcast::<NSButton>().ok())
        .find(|button| button.tag() == tag)
        .expect("toolbar button");
    unsafe {
        button.performClick(None);
    }
}

fn wait(viewer: &Viewer, predicate: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while viewer
        .javascript(&format!("return !!({predicate});"))
        .unwrap()
        != "1"
    {
        assert!(
            Instant::now() < deadline,
            "did not reach {predicate}: {}",
            viewer
                .javascript("return document.URL + document.body.innerText;")
                .unwrap()
        );
        viewer.tick();
    }
}

pub(crate) fn run() {
    let fixture = tempfile::tempdir().unwrap();
    let config = fixture.path().join("config/flowmux");
    std::fs::create_dir_all(&config).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", config.parent().unwrap());
    std::fs::write(
        config.join("options.json"),
        r##"{"theme":"nord","theme_overrides":{"background":"#123456","foreground":"#fedcba"}}"##,
    )
    .unwrap();
    let path = fixture.path().join("한글 문서.md");
    std::fs::write(&path, include_str!("../../tests/fixtures/mermaid.md")).unwrap();
    let args = Args::parse([path.to_string_lossy().to_string()]).unwrap();
    let viewer = Viewer::new(args, true);
    viewer.ready().unwrap();
    assert!(viewer.window.isVisible());
    check_theme(
        &viewer,
        [18, 52, 86],
        "rgb(254, 220, 186)",
        "NSAppearanceNameDarkAqua",
    );
    wait(&viewer, "document.querySelectorAll('.mermaid-chart > svg').length === 4 && document.querySelectorAll('.mermaid-error').length === 1");
    wait(
        &viewer,
        "document.querySelector('.mermaid-chart').textContent.includes('시작')",
    );
    wait(
        &viewer,
        "document.querySelectorAll('code.language-mermaid').length === 1",
    );
    wait(&viewer, "document.querySelector('code.language-rust').textContent.trim() === 'let value = \"<tag>\";'");
    wait(&viewer, "document.querySelectorAll('.mermaid-chart > svg').length === new Set([...document.querySelectorAll('.mermaid-chart > svg')].map(s => s.id)).size");
    wait(
        &viewer,
        "document.querySelector('.mermaid-chart svg').getBoundingClientRect().height > 20",
    );
    assert_eq!(
        viewer
            .javascript("return document.documentElement.dataset.executed || 'safe';")
            .unwrap(),
        "safe"
    );
    viewer.javascript("document.querySelector('#script-link').click(); document.querySelector('#event-button').click(); return true;").unwrap();
    wait(&viewer, "document.querySelectorAll('.mermaid-chart script, .mermaid-chart [onerror], .mermaid-chart [onclick]').length === 0");
    assert_eq!(
        viewer
            .javascript("return document.documentElement.dataset.executed || 'safe';")
            .unwrap(),
        "safe"
    );

    viewer
        .javascript(
            "window.originalDiagram = document.querySelector('.mermaid-chart > svg'); return true;",
        )
        .unwrap();
    click(&viewer, 5);
    assert!(unsafe { viewer.state.borrow().web.pageZoom() } > 1.0);
    viewer
        .javascript("window.scrollTo(0, 120); return true;")
        .unwrap();
    let scroll = viewer.javascript("return window.scrollY;").unwrap();
    std::fs::write(config.join("options.json"), r#"{"theme":"github-light"}"#).unwrap();
    viewer.poll();
    check_theme(
        &viewer,
        [255, 255, 255],
        "rgb(36, 41, 47)",
        "NSAppearanceNameAqua",
    );
    assert!(unsafe { viewer.state.borrow().web.pageZoom() } > 1.0);
    assert_eq!(viewer.javascript("return window.scrollY;").unwrap(), scroll);
    wait(
        &viewer,
        "window.originalDiagram === document.querySelector('.mermaid-chart > svg')",
    );
    std::fs::write(config.join("options.json"), "{}").unwrap();
    std::fs::write(
        config.join("theme"),
        "background = #112233\nforeground = #abcdef\n",
    )
    .unwrap();
    viewer.poll();
    check_theme(
        &viewer,
        [17, 34, 51],
        "rgb(171, 205, 239)",
        "NSAppearanceNameDarkAqua",
    );
    std::fs::remove_file(config.join("theme")).unwrap();
    viewer.poll();
    check_theme(
        &viewer,
        [40, 44, 52],
        "rgb(255, 255, 255)",
        "NSAppearanceNameDarkAqua",
    );
    click(&viewer, 4);
    assert_eq!(unsafe { viewer.state.borrow().web.pageZoom() }, 1.0);
    click(&viewer, 3);
    assert!(unsafe { viewer.state.borrow().web.pageZoom() } < 1.0);

    // Atomic replacement, deletion and recreation are common editor save paths.
    let replacement = fixture.path().join("replacement.md");
    std::fs::write(
        &replacement,
        "# Updated\n\n```mermaid\ngraph LR; A[갱신 완료]-->B\n```\n",
    )
    .unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    viewer.state.borrow_mut().poll();
    viewer.ready().unwrap();
    wait(&viewer, "document.querySelector('h1').textContent === 'Updated' && document.querySelector('.mermaid-chart svg').textContent.includes('갱신 완료')");
    assert!(
        unsafe { viewer.state.borrow().web.pageZoom() } < 1.0,
        "reload preserves zoom"
    );
    std::fs::remove_file(&path).unwrap();
    viewer.state.borrow_mut().poll();
    wait(
        &viewer,
        "document.querySelector('h1')?.textContent === 'Markdown render error'",
    );
    check_theme(
        &viewer,
        [40, 44, 52],
        "rgb(255, 255, 255)",
        "NSAppearanceNameDarkAqua",
    );
    std::fs::write(&path, "# Recreated\n\n[Next](next.html)\n").unwrap();
    viewer.state.borrow_mut().poll();
    viewer.ready().unwrap();
    wait(
        &viewer,
        "document.querySelector('h1').textContent === 'Recreated'",
    );
    viewer.state.borrow_mut().args.watch = false;
    std::fs::write(&path, "# No watch\n\n[Next](next.html)\n").unwrap();
    viewer.state.borrow_mut().poll();
    wait(
        &viewer,
        "document.querySelector('h1').textContent === 'Recreated'",
    );
    click(&viewer, 2);
    viewer.ready().unwrap();
    wait(
        &viewer,
        "document.querySelector('h1').textContent === 'No watch'",
    );

    std::fs::write(
        fixture.path().join("next.html"),
        "<h1>Next page</h1><script>document.documentElement.dataset.executed='bad'</script>",
    )
    .unwrap();
    viewer
        .javascript("document.querySelector('a[href=\"next.html\"]').click(); return true;")
        .unwrap();
    wait(
        &viewer,
        "document.querySelector('h1')?.textContent === 'Next page'",
    );
    assert_eq!(
        viewer
            .javascript("return document.documentElement.dataset.executed || 'safe';")
            .unwrap(),
        "safe"
    );
    click(&viewer, 0);
    wait(
        &viewer,
        "document.querySelector('h1')?.textContent === 'No watch'",
    );
    click(&viewer, 1);
    wait(
        &viewer,
        "document.querySelector('h1')?.textContent === 'Next page'",
    );
    click(&viewer, 2);
    viewer.ready().unwrap();
    wait(
        &viewer,
        "document.querySelector('h1').textContent === 'No watch'",
    );
    let close = objc2_app_kit::NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        objc2_app_kit::NSEventType::KeyDown, NSPoint::new(0.0, 0.0), NSEventModifierFlags::Command,
        0.0, viewer.window.windowNumber(), None, &NSString::from_str("w"), &NSString::from_str("w"), false, 13).unwrap();
    assert!(viewer
        .app
        .mainMenu()
        .expect("native application menu")
        .performKeyEquivalent(&close));
    assert!(!viewer.window.isVisible());
    println!("MACOS_MARKDOWN_NATIVE_OK");
}
