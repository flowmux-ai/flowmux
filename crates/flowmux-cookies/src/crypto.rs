// SPDX-License-Identifier: GPL-3.0-or-later
use crate::import::ImportError;
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
    ) -> Result<String, ImportError> {
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
        decrypt_value(&bytes[3..], key, host, version).map_err(Into::into)
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
fn storage_password(browser: BrowserId) -> Result<Zeroizing<Vec<u8>>, ImportError> {
    use dbus_secret_service::{EncryptionType, SecretService};
    // shortcut: expired OS prompts need manual closure until GNOME Keyring handles Dismiss safely.
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
        .map_err(|_| "Keyring approval was canceled, denied, or timed out. Close any remaining system keyring prompt before retrying.".to_string())?;
    item.get_secret()
        .map(Zeroizing::new)
        .map_err(|_| "The system keyring denied access to the browser's encryption key".into())
}

#[cfg(target_os = "macos")]
fn storage_password(browser: BrowserId) -> Result<Zeroizing<Vec<u8>>, ImportError> {
    let name = match browser {
        BrowserId::Chrome => "Chrome",
        BrowserId::Chromium => "Chromium",
        BrowserId::Brave => "Brave",
        BrowserId::Edge => "Microsoft Edge",
        BrowserId::Arc => "Arc",
        BrowserId::Firefox => return Err("Firefox does not use a Chromium key".into()),
    };
    let service = format!("{name} Safe Storage");
    // Search the user's keychain list, not just the default keychain. Security
    // displays the native access/unlock prompt; never change the item's ACL.
    security_framework::os::macos::passwords::find_generic_password(None, &service, name)
        .map(|(password, _)| Zeroizing::new(password.to_vec()))
        .map_err(|error| keychain_error(&service, error.code()))
}

#[cfg(target_os = "macos")]
fn keychain_error(service: &str, code: i32) -> ImportError {
    let guidance = match code {
        -128 => "Keychain approval was canceled. Retry when ready and choose Allow in the macOS prompt.",
        -25293 => "Keychain access was denied. Retry and approve access in the macOS prompt. If no prompt appears, inspect this item's Access Control in Keychain Access.",
        -25300 => "The browser's encryption key was not found. Open the source browser once and unlock its keychain in Keychain Access before retrying.",
        -25308 | -25315 => "macOS could not show the Keychain prompt. Unlock your Mac and the browser's keychain, then retry from the Flowmux window.",
        _ => "Open Keychain Access, unlock the browser's keychain, then retry and approve the macOS prompt.",
    };
    ImportError::Keychain(format!("{service}: {guidance}\n\nmacOS status: {code}. Enter your keychain password only in the macOS system prompt. Full Disk Access does not grant Keychain access."))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn storage_password(_: BrowserId) -> Result<Zeroizing<Vec<u8>>, ImportError> {
    Err("Cookie import is not supported on this operating system".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_failures_explain_the_required_recovery_without_secrets() {
        for (status, expected) in [
            (-128, "canceled"),
            (-25293, "denied"),
            (-25300, "not found"),
            (-25308, "could not show"),
            (-25315, "could not show"),
            (-25291, "unlock"),
        ] {
            let error = keychain_error("Chrome Safe Storage", status);
            assert!(matches!(error, ImportError::Keychain(_)));
            let message = error.to_string();
            assert!(message.contains(expected));
            assert!(message.contains(&status.to_string()));
            assert!(message.contains("Full Disk Access does not grant Keychain access"));
        }
    }

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
            .to_string()
            .contains("does not support"));
    }
}
