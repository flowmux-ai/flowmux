// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded history pages pinned to the first page's HEAD.
use super::{args, git, repository_root, revision};
use std::path::Path;

pub const PAGE_SIZE: usize = 50;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub oid: String,
    pub subject: String,
}

#[derive(Debug)]
pub struct Page {
    pub tip: Option<String>,
    pub commits: Vec<Commit>,
    pub has_more: bool,
}

pub fn load(start: &Path, tip: Option<&str>, offset: usize) -> Result<Page, String> {
    let root = repository_root(start)?;
    let tip = match tip {
        Some(tip) => revision(&root, &format!("{tip}^{{commit}}"))?,
        None => {
            // --revs-only emits nothing for an unborn HEAD, but Git failures
            // still propagate instead of being presented as an empty history.
            let head = git(&root, &args(&["rev-parse", "--revs-only", "HEAD"]), false)?;
            let head = String::from_utf8_lossy(&head).trim().to_string();
            if head.is_empty() {
                return Ok(Page {
                    tip: None,
                    commits: Vec::new(),
                    has_more: false,
                });
            }
            revision(&root, &format!("{head}^{{commit}}"))?
        }
    };
    let bytes = git(
        &root,
        &args(&[
            "log",
            "--date-order",
            "--no-show-signature",
            "--format=%H%x00%s",
            "-z",
            &format!("--max-count={}", PAGE_SIZE + 1),
            &format!("--skip={offset}"),
            &tip,
            "--",
        ]),
        false,
    )?;
    let mut fields = bytes.split(|b| *b == 0);
    let mut commits = Vec::new();
    while let Some(oid) = fields.next().filter(|oid| !oid.is_empty()) {
        let subject = fields.next().ok_or("Incomplete Git history")?;
        commits.push(Commit {
            oid: String::from_utf8(oid.to_vec()).map_err(|_| "Invalid commit ID")?,
            subject: String::from_utf8_lossy(subject)
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect(),
        });
    }
    let has_more = commits.len() > PAGE_SIZE;
    commits.truncate(PAGE_SIZE);
    Ok(Page {
        tip: Some(tip),
        commits,
        has_more,
    })
}
