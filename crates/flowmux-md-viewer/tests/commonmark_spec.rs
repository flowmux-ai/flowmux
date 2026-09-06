// SPDX-License-Identifier: GPL-3.0-or-later

use flowmux_md_viewer::render_markdown_body;

mod support;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CommonMarkExample {
    markdown: String,
    html: String,
    example: u32,
}

#[test]
fn renders_commonmark_0_31_2_spec_examples_to_html() {
    let examples: Vec<CommonMarkExample> =
        serde_json::from_str(include_str!("fixtures/commonmark-0.31.2-spec.json"))
            .expect("parse CommonMark spec fixture");
    assert_eq!(examples.len(), 652);

    let differences = support::expected_differences();
    for example in examples {
        let html = render_markdown_body(&example.markdown);
        if let Some(expected) = differences.get(&example.markdown) {
            assert!(
                expected.html.contains(&html),
                "example {}: {}\nexpected: {:?}\nactual: {html:?}",
                example.example,
                expected.reason,
                expected.html
            );
        } else {
            assert_eq!(html, example.html, "example {}", example.example);
        }
    }
}
