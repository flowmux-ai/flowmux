// SPDX-License-Identifier: GPL-3.0-or-later
use serde::{Deserialize, Serialize};

pub const PAGE_SIZE: usize = 500;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Match {
    pub id: u32,
    pub line: u32,
    pub column: u16,
    pub preview: String,
}
pub fn validate_query(query: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !query.is_empty()
            && query.encode_utf16().count() <= 1024
            && !query.contains(['\r', '\n', '\0']),
        "use a nonempty single-line query of at most 1024 characters"
    );
    Ok(())
}
