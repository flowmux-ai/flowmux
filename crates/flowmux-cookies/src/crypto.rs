// SPDX-License-Identifier: GPL-3.0-or-later
use crate::source::BrowserId;
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub(crate) struct Decryptor {
    browser: BrowserId,
    key: Option<Zeroizing<[u8; 16]>>,
}

impl Decryptor {
    pub(crate) fn new(browser: BrowserId) -> Self {
        Self { browser, key: None }
    }

    pub(crate) fn decrypt(
        &mut self,
        bytes: &[u8],
        host: &str,
        version: i64,
    ) -> Result<String, String> {
        let prefix = bytes.get(..3).ok_or("Invalid encrypted cookie")?;
        if prefix != b"v10" && prefix != b"v11" {
            return Err("This browser uses a cookie encryption format Flowmux does not support. Sign in inside Flowmux instead.".into());
        }
        #[cfg(target_os = "linux")]
        let legacy = prefix == b"v10";
        #[cfg(not(target_os = "linux"))]
        let legacy = false;
        if !legacy && self.key.is_none() {
            let password = storage_password(self.browser)?;
            self.key = Some(derive_key(
                &password,
                if cfg!(target_os = "macos") { 1003 } else { 1 },
            ));
        }
        let legacy_key = derive_key(b"peanuts", 1);
        let key = if legacy {
            &legacy_key
        } else {
            self.key.as_ref().unwrap()
        };
        decrypt_value(&bytes[3..], key, host, version)
    }
}

fn derive_key(password: &[u8], iterations: u32) -> Zeroizing<[u8; 16]> {
    let mut key = Zeroizing::new([0; 16]);
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password, b"saltysalt", iterations, key.as_mut());
    key
}

fn decrypt_value(bytes: &[u8], key: &[u8; 16], host: &str, version: i64) -> Result<String, String> {
    let plaintext = Zeroizing::new(
        cbc::Decryptor::<aes::Aes128>::new(key.into(), (&[b' '; 16]).into())
            .decrypt_padded_vec_mut::<Pkcs7>(bytes)
            .map_err(|_| "Could not decrypt cookies with the selected browser's key")?,
    );
    let value = if version >= 24 {
        let digest = Sha256::digest(host.as_bytes());
        if plaintext.get(..32) != Some(digest.as_slice()) {
            return Err("Cookie domain verification failed; nothing was imported".into());
        }
        &plaintext[32..]
    } else {
        &plaintext[..]
    };
    String::from_utf8(value.to_vec()).map_err(|_| "Cookie value is not valid UTF-8".into())
}

#[cfg(target_os = "linux")]
fn storage_password(browser: BrowserId) -> Result<Zeroizing<Vec<u8>>, String> {
    use dbus_secret_service::{EncryptionType, SecretService};
    let service =
        SecretService::connect_with_max_prompt_timeout(EncryptionType::Dh, 120).map_err(|_| {
            "Could not access the system keyring. Unlock or enable Secret Service and retry."
                .to_string()
        })?;
    let application = match browser {
        BrowserId::Edge => "microsoft-edge",
        other => other.slug(),
    };
    let found = service
        .search_items(std::collections::HashMap::from([(
            "application",
            application,
        )]))
        .map_err(|_| "Could not find the browser's encryption key".to_string())?;
    let mut items = found.unlocked;
    items.extend(found.locked);
    if items.len() != 1 {
        return Err("The browser's encryption key is missing or ambiguous. Unlock the browser's keyring and retry.".into());
    }
    let item = &items[0];
    item.unlock()
        .map_err(|_| "Keyring approval was canceled, denied, or timed out".to_string())?;
    item.get_secret()
        .map(Zeroizing::new)
        .map_err(|_| "The system keyring denied access to the browser's encryption key".into())
}

#[cfg(target_os = "macos")]
fn storage_password(browser: BrowserId) -> Result<Zeroizing<Vec<u8>>, String> {
    let name = match browser {
        BrowserId::Chrome => "Chrome",
        BrowserId::Chromium => "Chromium",
        BrowserId::Brave => "Brave",
        BrowserId::Edge => "Microsoft Edge",
        BrowserId::Arc => "Arc",
        BrowserId::Firefox => return Err("Firefox does not use a Chromium key".into()),
    };
    keyring::Entry::new(&format!("{name} Safe Storage"), name)
        .and_then(|entry| entry.get_secret())
        .map(Zeroizing::new)
        .map_err(|_| "Keychain access was denied or the browser key was not found. Allow access in the macOS prompt and retry.".into())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn storage_password(_: BrowserId) -> Result<Zeroizing<Vec<u8>>, String> {
    Err("Cookie import is not supported on this operating system".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;

    #[test]
    fn decrypts_chromium_and_verifies_domain_binding() {
        let key = derive_key(b"fixture-password", 1003);
        let host = ".example.test";
        let mut value = Sha256::digest(host.as_bytes()).to_vec();
        value.extend_from_slice(b"fixture-session");
        let encrypted = cbc::Encryptor::<aes::Aes128>::new((&*key).into(), (&[b' '; 16]).into())
            .encrypt_padded_vec_mut::<Pkcs7>(&value);
        assert_eq!(
            decrypt_value(&encrypted, &key, host, 24).unwrap(),
            "fixture-session"
        );
        assert!(decrypt_value(&encrypted, &key, ".other.test", 24).is_err());
        assert!(decrypt_value(&encrypted, &derive_key(b"wrong", 1003), host, 24).is_err());
        assert!(decrypt_value(&encrypted[..5], &key, host, 24).is_err());
    }

    #[test]
    fn unknown_encryption_does_not_request_a_key() {
        assert!(Decryptor::new(BrowserId::Chrome)
            .decrypt(b"v20fixture", "example.test", 24)
            .unwrap_err()
            .contains("does not support"));
    }
}
