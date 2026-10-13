//! Windows DPAPI helpers for at-rest API keys in the local SQLite DB.
//! Ciphertext is stored as `enc:v1:<base64>`; plaintext (legacy) is left as-is
//! until the next save, which re-encrypts.

#[cfg(windows)]
use base64::{engine::general_purpose::STANDARD as B64, Engine};
#[cfg(windows)]
use windows::Win32::Foundation::{LocalFree, HLOCAL};
#[cfg(windows)]
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB,
};

const PREFIX: &str = "enc:v1:";

/// Encrypt a secret for local storage. Non-Windows builds return the input unchanged.
pub fn protect_secret(plain: &str) -> Result<String, String> {
    if plain.is_empty() || plain.starts_with(PREFIX) {
        return Ok(plain.to_string());
    }
    #[cfg(windows)]
    {
        protect_windows(plain)
    }
    #[cfg(not(windows))]
    {
        Ok(plain.to_string())
    }
}

/// Decrypt a secret loaded from storage. Plaintext (no prefix) passes through.
pub fn unprotect_secret(stored: &str) -> Result<String, String> {
    if stored.is_empty() || !stored.starts_with(PREFIX) {
        return Ok(stored.to_string());
    }
    #[cfg(windows)]
    {
        unprotect_windows(stored)
    }
    #[cfg(not(windows))]
    {
        Ok(stored.to_string())
    }
}

#[cfg(windows)]
fn protect_windows(plain: &str) -> Result<String, String> {
    let mut bytes = plain.as_bytes().to_vec();
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_mut_ptr(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: CryptProtectData allocates out_blob; freed with LocalFree below.
    unsafe {
        CryptProtectData(
            &in_blob,
            windows::core::PCWSTR::null(),
            None,
            None,
            None,
            0,
            &mut out_blob,
        )
        .map_err(|e| format!("DPAPI protect failed: {e}"))?;
    }
    let slice = unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize) };
    let encoded = B64.encode(slice);
    unsafe {
        let _ = LocalFree(Some(HLOCAL(out_blob.pbData as *mut _)));
    }
    Ok(format!("{PREFIX}{encoded}"))
}

#[cfg(windows)]
fn unprotect_windows(stored: &str) -> Result<String, String> {
    let b64 = stored.strip_prefix(PREFIX).unwrap_or(stored);
    let mut bytes = B64
        .decode(b64)
        .map_err(|e| format!("DPAPI ciphertext corrupt: {e}"))?;
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_mut_ptr(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    unsafe {
        CryptUnprotectData(&in_blob, None, None, None, None, 0, &mut out_blob)
            .map_err(|e| format!("DPAPI unprotect failed: {e}"))?;
    }
    let slice = unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize) };
    let plain = String::from_utf8_lossy(slice).to_string();
    unsafe {
        let _ = LocalFree(Some(HLOCAL(out_blob.pbData as *mut _)));
    }
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_already_prefixed_passthrough() {
        assert_eq!(protect_secret("").unwrap(), "");
        let fake = format!("{PREFIX}abc");
        assert_eq!(protect_secret(&fake).unwrap(), fake);
        assert_eq!(unprotect_secret("plain-key").unwrap(), "plain-key");
    }

    #[cfg(windows)]
    #[test]
    fn roundtrip_windows() {
        let cipher = protect_secret("sk-test-secret").unwrap();
        assert!(cipher.starts_with(PREFIX));
        assert_eq!(unprotect_secret(&cipher).unwrap(), "sk-test-secret");
    }
}
