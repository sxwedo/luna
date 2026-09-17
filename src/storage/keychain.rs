use anyhow::{Context, Result};
use base64::Engine;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct KeychainGoogleToken {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub token_type: Option<String>,
    pub expiry: Option<String>,
}

/// Helper to read passwords from macOS Keychain.
pub struct KeychainReader;

impl KeychainReader {
    #[cfg(target_os = "macos")]
    pub fn read_generic_password(service: &str, account: &str) -> Result<Option<Vec<u8>>> {
        use security_framework::passwords::get_generic_password;
        match get_generic_password(service, account) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) => {
                let code = e.code();
                // errSecItemNotFound = -25300
                if code == -25300 {
                    Ok(None)
                } else {
                    Err(anyhow::anyhow!("Keychain error {}: {}", code, e))
                }
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn read_generic_password(_service: &str, _account: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Read and parse Antigravity credentials stored by the official IDE.
    pub fn read_antigravity_token() -> Result<Option<KeychainGoogleToken>> {
        let raw_bytes = match Self::read_generic_password("gemini", "antigravity")? {
            Some(b) => b,
            None => return Ok(None),
        };

        let raw_str = std::str::from_utf8(&raw_bytes)
            .context("Invalid UTF-8 in Keychain secret")?
            .trim();

        let json_bytes = if let Some(encoded) = raw_str.strip_prefix("go-keyring-base64:") {
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .context("Failed to decode base64 from go-keyring entry")?
        } else {
            raw_str.as_bytes().to_vec()
        };

        // Try direct parse, or parse nested under "token" field
        let value: serde_json::Value = serde_json::from_slice(&json_bytes)?;
        let target = if let Some(token_obj) = value.get("token") {
            token_obj.clone()
        } else {
            value
        };

        let parsed: KeychainGoogleToken =
            serde_json::from_value(target).context("Failed to map json to KeychainGoogleToken")?;

        Ok(Some(parsed))
    }
}
