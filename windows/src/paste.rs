// SPDX-License-Identifier: GPL-3.0-or-later
//! Text paste limits shared by the Windows CLI and trusted terminal bridge.
use serde::{Deserialize, Serialize};

// Even six-byte JSON escaping of every byte leaves room in the 1 MiB frame.
pub const MAX_TEXT_BYTES: usize = 128 * 1024;

pub fn validate(text: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        text.len() <= MAX_TEXT_BYTES,
        "paste exceeds 128 KiB of UTF-8 text"
    );
    anyhow::ensure!(!text.contains('\0'), "paste text contains NUL");
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Ok { data: String, bracketed: bool },
    Error { message: String },
}

impl Outcome {
    pub fn into_input(self) -> anyhow::Result<(Vec<u8>, bool)> {
        match self {
            Self::Ok { data, bracketed } => {
                let text = if bracketed {
                    data.strip_prefix("\x1b[200~")
                        .and_then(|s| s.strip_suffix("\x1b[201~"))
                        .ok_or_else(|| anyhow::anyhow!("invalid paste brackets"))?
                } else {
                    &data
                };
                validate(text)?;
                Ok((data.into_bytes(), bracketed))
            }
            Self::Error { message } => {
                anyhow::ensure!(message.len() <= 512, "invalid paste error");
                anyhow::bail!(message)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_use_bytes_and_preserve_unicode_and_control_text() {
        let text = "한글 한 😀\r\n\t\x1b[201~";
        let data = format!("\x1b[200~{text}\x1b[201~");
        let (bytes, bracketed) = Outcome::Ok {
            data: data.clone(),
            bracketed: true,
        }
        .into_input()
        .unwrap();
        assert!(bracketed);
        assert_eq!(bytes, data.as_bytes());
        assert!(validate(&"a".repeat(MAX_TEXT_BYTES)).is_ok());
        assert!(validate(&"a".repeat(MAX_TEXT_BYTES + 1)).is_err());
        assert!(validate(&"한".repeat(MAX_TEXT_BYTES / 3 + 1)).is_err());
        assert!(validate("한\0글").is_err());
        assert!(Outcome::Ok {
            data: "한글".into(),
            bracketed: true
        }
        .into_input()
        .is_err());
    }

    #[test]
    fn worst_case_escaped_response_fits_bridge() {
        let identity = crate::protocol::Identity::new(uuid::Uuid::new_v4());
        let mut value = serde_json::to_value(identity).unwrap();
        value["message"] = serde_json::json!({"type":"pasted","request":uuid::Uuid::new_v4(),
            "sequence":u64::MAX,"outcome":{"status":"ok","bracketed":true,
            "data":format!("\x1b[200~{}\x1b[201~", "\x01".repeat(MAX_TEXT_BYTES))}});
        assert!(value.to_string().len() < crate::protocol::MAX_MESSAGE_BYTES);
    }
}
