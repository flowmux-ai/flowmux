// SPDX-License-Identifier: GPL-3.0-or-later
//! Chromium v10/v11 cookies: PBKDF2-HMAC-SHA1 and AES-128-CBC.
use crate::{
    cookie::{Cookie, SameSite},
    profiles::{self, Profile},
    source::{BrowserId, Error, Source},
};
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub struct Chromium {
    id: BrowserId,
}
impl Chromium {
    pub fn new(id: BrowserId) -> Self {
        Self { id }
    }
}
impl Source for Chromium {
    fn id(&self) -> BrowserId {
        self.id
    }
    fn detect(&self) -> Option<PathBuf> {
        profiles::discover()
            .ok()?
            .iter()
            .filter(|p| p.browser == self.id)
            .map(profiles::cookie_path)
            .find(|p| p.is_file())
    }
    fn list_cookies(&self, domain: Option<&str>) -> Result<Vec<Cookie>, Error> {
        Ok(read_cookies(&profiles::resolve(self.id, None)?, domain)?.cookies)
    }
}

pub fn read_cookies(
    profile: &Profile,
    domain: Option<&str>,
) -> Result<profiles::CookieBatch, Error> {
    read_with_password(profile, domain, || password(profile.browser))
}

fn read_with_password(
    profile: &Profile,
    domain: Option<&str>,
    password: impl FnOnce() -> Result<Vec<u8>, Error>,
) -> Result<profiles::CookieBatch, Error> {
    let db = profiles::database(&profiles::cookie_path(profile))?;
    let version: i64 = db
        .query_row("SELECT value FROM meta WHERE key = 'version'", [], |r| {
            r.get::<_, String>(0)
        })?
        .parse()
        .map_err(|_| Error::Import("invalid Chromium cookie database version".into()))?;
    let mut columns = db.prepare("PRAGMA table_info(cookies)")?;
    let columns = columns
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    let partition = if columns.iter().any(|c| c == "top_frame_site_key") {
        "top_frame_site_key"
    } else {
        "''"
    };
    let sql = format!("SELECT host_key, name, value, encrypted_value, path, expires_utc, is_secure, is_httponly, samesite, {partition} FROM cookies");
    let mut statement = db.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let mut key = None;
    let mut password = Some(password);
    let mut batch = profiles::CookieBatch::default();
    while let Some(row) = rows.next()? {
        let host: String = row.get(0)?;
        if !profiles::domain_matches(&host, domain) {
            continue;
        }
        let expires: i64 = row.get(5)?;
        let expires_at = if expires == 0 {
            None
        } else {
            Some(
                chrono::DateTime::from_timestamp(expires / 1_000_000 - 11_644_473_600, 0)
                    .ok_or_else(|| Error::Import("invalid cookie expiration".into()))?,
            )
        };
        if expires_at.is_some_and(|date| date <= chrono::Utc::now()) {
            batch.skipped_expired += 1;
            continue;
        }
        if !row.get::<_, String>(9)?.is_empty() {
            batch.skipped_isolated += 1;
            continue;
        }
        let encrypted: Vec<u8> = row.get(3)?;
        let value = if encrypted.is_empty() {
            row.get(2)?
        } else {
            #[cfg(target_os = "macos")]
            if !encrypted.starts_with(b"v10") {
                return Err(Error::EncryptedValuesUnsupported);
            }
            #[cfg(target_os = "macos")]
            let needs_secret = encrypted.starts_with(b"v10");
            #[cfg(not(target_os = "macos"))]
            let needs_secret = encrypted.starts_with(b"v11");
            if !encrypted.starts_with(b"v10") && !encrypted.starts_with(b"v11") {
                return Err(Error::EncryptedValuesUnsupported);
            }
            if needs_secret && key.is_none() {
                let secret = password.take().unwrap()()?;
                key = Some(derive_key(
                    &secret,
                    if cfg!(target_os = "macos") { 1003 } else { 1 },
                ));
            }
            #[cfg(target_os = "macos")]
            let selected = key.as_ref().ok_or(Error::EncryptedValuesUnsupported)?;
            #[cfg(not(target_os = "macos"))]
            let legacy_key = derive_key(b"peanuts", 1);
            #[cfg(not(target_os = "macos"))]
            let selected = if needs_secret {
                key.as_ref().unwrap()
            } else {
                &legacy_key
            };
            decrypt(&encrypted, selected, &host, version)?
        };
        batch.cookies.push(Cookie {
            host,
            name: row.get(1)?,
            value,
            path: row.get(4)?,
            expires_at,
            secure: row.get::<_, i64>(6)? != 0,
            http_only: row.get::<_, i64>(7)? != 0,
            same_site: match row.get::<_, i64>(8)? {
                // Chromium applies Lax when the source cookie has no explicit policy.
                -1 | 1 => SameSite::Lax,
                0 => SameSite::None,
                2 => SameSite::Strict,
                _ => return Err(Error::Import("unknown cookie SameSite policy".into())),
            },
        });
    }
    Ok(batch)
}

fn derive_key(secret: &[u8], iterations: u32) -> [u8; 16] {
    let mut key = [0; 16];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA1,
        std::num::NonZeroU32::new(iterations).unwrap(),
        b"saltysalt",
        secret,
        &mut key,
    );
    key
}

fn decrypt(encrypted: &[u8], key: &[u8; 16], host: &str, version: i64) -> Result<String, Error> {
    let failure = || {
        Error::Import("could not decrypt cookie; key, format or domain hash does not match".into())
    };
    let payload = encrypted.get(3..).ok_or_else(failure)?;
    let mut plain = cbc::Decryptor::<aes::Aes128>::new(key.into(), &[b' '; 16].into())
        .decrypt_padded_vec_mut::<Pkcs7>(payload)
        .map_err(|_| failure())?;
    if version >= 24 {
        if plain.get(..32) != Some(Sha256::digest(host.as_bytes()).as_slice()) {
            return Err(failure());
        }
        plain.drain(..32);
    }
    String::from_utf8(plain).map_err(|_| failure())
}

#[cfg(target_os = "macos")]
fn password(browser: BrowserId) -> Result<Vec<u8>, Error> {
    let (service, account) = match browser {
        BrowserId::Chrome => ("Chrome Safe Storage", "Chrome"),
        BrowserId::Chromium => ("Chromium Safe Storage", "Chromium"),
        BrowserId::Brave => ("Brave Safe Storage", "Brave"),
        BrowserId::Edge => ("Microsoft Edge Safe Storage", "Microsoft Edge"),
        BrowserId::Arc => ("Arc Safe Storage", "Arc"),
        BrowserId::Firefox => return Err(Error::EncryptedValuesUnsupported),
    };
    security_framework::passwords::get_generic_password(service, account).map_err(|e| {
        Error::Import(format!(
            "could not access {service} in Keychain ({e}); allow access to import cookies"
        ))
    })
}

#[cfg(target_os = "linux")]
fn password(browser: BrowserId) -> Result<Vec<u8>, Error> {
    use dbus_secret_service::{EncryptionType, SecretService};
    let application = match browser {
        BrowserId::Chrome => "chrome",
        BrowserId::Chromium => "chromium",
        BrowserId::Brave => "brave",
        BrowserId::Edge => "microsoft-edge",
        BrowserId::Arc => "arc",
        BrowserId::Firefox => return Err(Error::EncryptedValuesUnsupported),
    };
    let failure = |e| {
        Error::Import(format!(
            "could not read browser key from Secret Service: {e}"
        ))
    };
    let service = SecretService::connect(EncryptionType::Dh).map_err(failure)?;
    let items = service
        .search_items(std::collections::HashMap::from([(
            "application",
            application,
        )]))
        .map_err(failure)?;
    if items.unlocked.len() > 1 {
        return Err(Error::Import(
            "multiple browser keys found in Secret Service; cannot select a key unambiguously"
                .into(),
        ));
    }
    let item = items.unlocked.first().ok_or_else(|| Error::Import("browser key is missing or locked in Secret Service; unlock the keyring first (KWallet is not supported)".into()))?;
    item.get_secret().map_err(failure)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn password(_: BrowserId) -> Result<Vec<u8>, Error> {
    Err(Error::EncryptedValuesUnsupported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;
    #[test]
    fn openssl_vector_decrypts_through_sqlite_reader() {
        // Independent PBKDF2/AES vector generated with Python hashlib and OpenSSL.
        #[cfg(target_os = "macos")]
        let hex = "7631309308e0850b08496feea365cd8abe62608a68ee479d36b0dd95478f05d05e4c1ce1582950eebea684ec16f9023932538d2a55ba5e470a8e1ddfbcb352d1bad46d";
        #[cfg(not(target_os = "macos"))]
        let hex = "763131e4cb7b8174bbd4aa4747dcee4a0c924b9da9530cfc8089a470e7e767e4b924d569079fa6b762b87d882b675e11e48d29267b03b8947333bf39efec93e0efa710";
        let encrypted: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(dir.path().join("Cookies")).unwrap();
        db.execute_batch("CREATE TABLE meta(key TEXT,value TEXT); INSERT INTO meta VALUES('version','24'); CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,encrypted_value BLOB,path TEXT,expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,samesite INTEGER);").unwrap();
        db.execute(
            "INSERT INTO cookies VALUES('.example.com','sid','',?1,'/',0,1,1,1)",
            [&encrypted],
        )
        .unwrap();
        let profile = Profile {
            browser: BrowserId::Chrome,
            name: "test".into(),
            path: dir.path().into(),
        };
        let result = read_with_password(&profile, None, || Ok(b"fixture-secret".to_vec())).unwrap();
        assert_eq!(result.cookies[0].value, "fixture-encrypted-session");
        assert!(read_with_password(&profile, None, || Ok(b"wrong-key".to_vec())).is_err());
        db.execute("UPDATE cookies SET encrypted_value=X'76323000'", [])
            .unwrap();
        assert!(matches!(
            read_with_password(&profile, None, || panic!(
                "unsupported format must not request the key"
            )),
            Err(Error::EncryptedValuesUnsupported)
        ));
    }

    #[test]
    fn decrypts_values_and_rejects_wrong_domain_key_and_truncation() {
        let key = derive_key(b"fixture-secret", 1003);
        for version in [23, 24] {
            let mut plain = Vec::new();
            if version == 24 {
                plain.extend(Sha256::digest(b".example.com"));
            }
            plain.extend("한글-session".as_bytes());
            let mut encrypted = b"v10".to_vec();
            encrypted.extend(
                cbc::Encryptor::<aes::Aes128>::new((&key).into(), &[b' '; 16].into())
                    .encrypt_padded_vec_mut::<Pkcs7>(&plain),
            );
            assert_eq!(
                decrypt(&encrypted, &key, ".example.com", version).unwrap(),
                "한글-session"
            );
            assert!(decrypt(&encrypted, &[0; 16], ".example.com", version).is_err());
            assert!(decrypt(&encrypted[..5], &key, ".example.com", version).is_err());
            if version == 24 {
                assert!(decrypt(&encrypted, &key, ".evil.test", version).is_err());
            }
        }
    }
    #[test]
    fn reads_sqlite_without_modifying_source_or_querying_key_for_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cookies");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE meta(key TEXT, value TEXT); INSERT INTO meta VALUES('version','24'); CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,encrypted_value BLOB,path TEXT,expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,top_frame_site_key TEXT); INSERT INTO cookies VALUES('.example.com','sid','fixture',X'','/',0,1,1,1,''),('.notexample.com','other','excluded',X'','/',0,0,0,0,'');").unwrap();
        drop(db);
        let before = std::fs::read(&path).unwrap();
        let profile = Profile {
            browser: BrowserId::Chrome,
            name: "test".into(),
            path: dir.path().into(),
        };
        let rows = read_with_password(&profile, Some("example.com"), || {
            panic!("plaintext needs no key")
        })
        .unwrap();
        assert_eq!(rows.cookies.len(), 1);
        assert!(rows.cookies[0].http_only);
        assert_eq!(rows.cookies[0].value, "fixture");
        assert_eq!(before, std::fs::read(path).unwrap());
    }
}
