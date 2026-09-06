// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::HashMap;

#[derive(serde::Deserialize)]
pub struct ExpectedDifference {
    pub reason: String,
    pub html: Vec<String>,
}

pub fn expected_differences() -> HashMap<String, ExpectedDifference> {
    serde_json::from_str(include_str!("../fixtures/flowmux-spec-differences.json")).unwrap()
}
