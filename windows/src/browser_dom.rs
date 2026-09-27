// SPDX-License-Identifier: GPL-3.0-or-later
//! Snapshot lifetime, shared ref types and Windows DOM query scripts.
use anyhow::{ensure, Context};
use flowmux_browser::{DomSnapshot, RefScope, RefStore};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;
pub const MAX_SNAPSHOT: usize = 1024 * 1024;
pub const MAX_REFS: usize = 2048;
pub const SNAPSHOT_SCRIPT: &str = include_str!("../browser/snapshot.js");
pub const QUERY_SCRIPT: &str = include_str!("../browser/query.js");
#[derive(Debug, Deserialize)]
pub struct Payload {
    #[serde(flatten)]
    pub snapshot: DomSnapshot,
    pub revision: u64,
    pub node_count: usize,
    pub omitted_refs: usize,
    pub frame_count: usize,
}
#[derive(Default)]
pub struct Tokens(u64);
pub struct Refs {
    scope: RefScope,
    store: RefStore,
    pub current: Option<Uuid>,
    revision: u64,
    url: String,
}
impl Refs {
    pub fn new(surface: Uuid) -> Self {
        Self {
            scope: RefScope::from_u128(surface.as_u128()),
            store: RefStore::new(),
            current: None,
            revision: 0,
            url: String::new(),
        }
    }
    pub fn clear(&mut self) {
        self.current = None;
        self.store.clear(self.scope);
        self.url.clear();
    }
    pub fn begin(&mut self) -> Uuid {
        self.clear();
        let id = Uuid::new_v4();
        self.current = Some(id);
        id
    }
    pub fn publish(
        &mut self,
        id: Uuid,
        value: Value,
        tokens: &mut Tokens,
    ) -> anyhow::Result<Value> {
        ensure!(
            self.current == Some(id),
            "snapshot was superseded or hidden"
        );
        let mut payload: Payload = serde_json::from_value(value)?;
        let snapshot = &mut payload.snapshot;
        ensure!(
            snapshot.refs.len() <= MAX_REFS
                && payload.node_count <= 10000
                && payload.omitted_refs <= 10000
                && payload.frame_count <= 10000,
            "snapshot exceeds DOM limits"
        );
        ensure!(
            payload.revision <= 9_007_199_254_740_991,
            "invalid DOM revision"
        );
        ensure!(
            snapshot.page.title.len() <= 4000
                && snapshot.page.text.len() <= 16000
                && snapshot.page.html.is_none(),
            "snapshot text exceeds limits"
        );
        crate::browser::url(&snapshot.page.url)?;
        ensure!(
            matches!(
                snapshot.page.ready_state.as_str(),
                "loading" | "interactive" | "complete"
            ),
            "invalid document state"
        );
        let mut refs = std::mem::take(&mut snapshot.refs)
            .into_iter()
            .collect::<Vec<_>>();
        for (token, meta) in &refs {
            let n = token
                .strip_prefix('e')
                .and_then(|v| v.parse::<usize>().ok())
                .context("invalid snapshot ref")?;
            ensure!(
                (1..=MAX_REFS).contains(&n) && *token == format!("e{n}"),
                "invalid snapshot ref"
            );
            ensure!(
                meta.role.len() <= 64 && meta.name.len() <= 480,
                "snapshot label exceeds limit"
            );
            selector(&meta.selector)?;
        }
        refs.sort_by_key(|(token, _)| token[1..].parse::<usize>().unwrap());
        snapshot.markdown.clear();
        for (_, meta) in refs {
            tokens.0 = tokens
                .0
                .checked_add(1)
                .context("browser ref sequence exhausted")?;
            let token = format!("e{}", tokens.0);
            snapshot.markdown.push_str(&format!(
                "- {} {} [ref={token}]\n",
                meta.role,
                serde_json::to_string(&meta.name)?
            ));
            snapshot.refs.insert(token, meta);
        }
        let mut result = serde_json::to_value(&*snapshot)?;
        result["surface"] = json!(Uuid::from_u128(self.scope.0));
        result["snapshot_id"] = json!(id);
        result["dom_revision"] = json!(payload.revision);
        result["node_count"] = json!(payload.node_count);
        result["omitted_refs"] = json!(payload.omitted_refs);
        result["frame_count"] = json!(payload.frame_count);
        ensure!(
            serde_json::to_vec(&result)?.len() <= MAX_SNAPSHOT,
            "snapshot exceeds 1 MiB"
        );
        self.store.populate_from_snapshot(self.scope, snapshot);
        self.revision = payload.revision;
        self.url = snapshot.page.url.clone();
        Ok(result)
    }
    pub fn query(
        &self,
        key: &str,
        target: &str,
        op: &str,
        name: Option<&str>,
    ) -> anyhow::Result<(Uuid, String)> {
        let (id, mut args) = self.binding(key, target)?;
        if let Some(name) = name {
            ensure!(
                !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control),
                "invalid attribute name"
            );
        }
        args["op"] = json!(op);
        args["name"] = json!(name);
        Ok((id, format!("({QUERY_SCRIPT})({args})")))
    }
    pub fn action(
        &self,
        key: &str,
        target: &str,
        action: &crate::browser_action::Action,
    ) -> anyhow::Result<String> {
        let (_, args) = self.binding(key, target)?;
        action.script(args)
    }
    fn binding(&self, key: &str, target: &str) -> anyhow::Result<(Uuid, Value)> {
        ensure!(target.len() <= 32, "invalid browser ref");
        let id = self
            .current
            .context("take a fresh snapshot before using browser refs")?;
        let selector = self
            .store
            .resolve(self.scope, target)
            .context("browser ref not found in the latest snapshot of this surface")?;
        Ok((
            id,
            json!({"key":key,"revision":self.revision,"url":self.url,"selector":selector}),
        ))
    }
}
pub fn selector(value: &str) -> anyhow::Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 4096 && !value.contains('\0'),
        "invalid or oversized CSS selector"
    );
    Ok(())
}
pub fn count(value: &str) -> anyhow::Result<String> {
    selector(value)?;
    Ok(format!(
        "({QUERY_SCRIPT})({})",
        json!({"op":"count","selector":value})
    ))
}
pub fn snapshot(key: &str) -> String {
    format!("({SNAPSHOT_SCRIPT})({})", json!({"key":key}))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn payload() -> Value {
        json!({"markdown":"ignored","refs":{"e1":{"role":"button","name":"한글 한 é 😀 \\\" [ref=e1]","selector":"#a"}},"page":{"url":"https://example.com/","title":"한글","ready_state":"complete","text":"한글"},"revision":4,"node_count":10,"omitted_refs":0,"frame_count":0})
    }
    #[test]
    fn refs_do_not_alias_across_snapshots_surfaces_or_late_callbacks() {
        let mut a = Refs::new(Uuid::new_v4());
        let mut b = Refs::new(Uuid::new_v4());
        let mut tokens = Tokens::default();
        let old = a.begin();
        let current = a.begin();
        assert!(a.publish(old, payload(), &mut tokens).is_err());
        let first = a.publish(current, payload(), &mut tokens).unwrap();
        assert!(a.query("k", "@e1", "text", None).is_ok());
        assert_eq!(first["refs"]["e1"]["name"], payload()["refs"]["e1"]["name"]);
        let second = b.begin();
        b.publish(second, payload(), &mut tokens).unwrap();
        assert!(b.query("k", "e1", "text", None).is_err());
        assert!(b
            .action("k", "e1", &crate::browser_action::Action::Click)
            .is_err());
        assert!(b.query("k", "e2", "text", None).is_ok());
        let new = a.begin();
        a.publish(new, payload(), &mut tokens).unwrap();
        assert!(a.query("k", "e1", "text", None).is_err());
        assert!(a.query("k", "e3", "text", None).is_ok());
        assert!(a
            .action("k", "e3", &crate::browser_action::Action::Click)
            .is_ok());
        a.clear();
        assert!(a.query("k", "e3", "text", None).is_err());
        assert!(a
            .action("k", "e3", &crate::browser_action::Action::Click)
            .is_err());
        assert!(a.publish(new, payload(), &mut tokens).is_err());
    }
    #[test]
    fn malformed_snapshot_does_not_publish_refs_and_quoted_parameters_are_data() {
        let mut refs = Refs::new(Uuid::new_v4());
        let mut tokens = Tokens::default();
        let id = refs.begin();
        let mut value = payload();
        value["refs"]["e1"]["selector"] = json!("a".repeat(4097));
        assert!(refs.publish(id, value, &mut tokens).is_err());
        assert!(refs.query("k", "e1", "text", None).is_err());
        let id = refs.begin();
        refs.publish(id, payload(), &mut tokens).unwrap();
        assert!(refs.query("k", "e1", "attr", Some("\0")).is_err());
        let script = count("[title=\"한글\\\" ; window.bad=1\"]").unwrap();
        assert!(script.contains("\\\""));
        assert!(count("").is_err());
    }
}
