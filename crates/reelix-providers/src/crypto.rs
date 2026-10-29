//! Secret handling for provider settings: encryption at rest, masking on read and
//! "mask means unchanged" merging on write.

use reelix_core::provider::SECRET_MASK;
use reelix_db::secrets::SecretBox;
use serde_json::{Map, Value};

use crate::error::{ProviderError, Result};

/// Prefix marking an encrypted settings value.
pub const ENC_PREFIX: &str = "enc:";

/// Encrypts one string value.
pub fn seal(secrets: &SecretBox, plain: &str) -> Result<String> {
    Ok(format!("{ENC_PREFIX}{}", secrets.encrypt(plain)?))
}

/// Decrypts a value produced by [`seal`]; plain values pass through unchanged.
pub fn open(secrets: &SecretBox, stored: &str) -> Result<String> {
    match stored.strip_prefix(ENC_PREFIX) {
        Some(enc) => Ok(secrets.decrypt(enc)?),
        None => Ok(stored.to_owned()),
    }
}

/// Encrypts every non-empty string in `secret_fields`.
pub fn encrypt_fields<'a>(
    secrets: &SecretBox,
    settings: &Map<String, Value>,
    secret_fields: impl IntoIterator<Item = &'a str>,
) -> Result<Map<String, Value>> {
    let mut out = settings.clone();
    for name in secret_fields {
        if let Some(Value::String(s)) = out.get(name)
            && !s.is_empty()
        {
            let sealed = seal(secrets, s)?;
            out.insert(name.to_owned(), Value::String(sealed));
        }
    }
    Ok(out)
}

/// Decrypts every `enc:` string (regardless of the current schema).
pub fn decrypt_all(secrets: &SecretBox, settings: &mut Map<String, Value>) -> Result<()> {
    for v in settings.values_mut() {
        if let Value::String(s) = v
            && s.starts_with(ENC_PREFIX)
        {
            *s = open(secrets, s).map_err(|e| {
                ProviderError::Validation(format!(
                    "cannot decrypt a stored secret (was the secret key changed?): {e}"
                ))
            })?;
        }
    }
    Ok(())
}

/// Replaces non-empty secret values with [`SECRET_MASK`].
pub fn mask<'a>(
    settings: &Map<String, Value>,
    secret_fields: impl IntoIterator<Item = &'a str>,
) -> Map<String, Value> {
    let mut out = settings.clone();
    for name in secret_fields {
        if let Some(Value::String(s)) = out.get_mut(name)
            && !s.is_empty()
        {
            *s = SECRET_MASK.to_owned();
        }
    }
    out
}

/// Where the incoming value is the mask, keeps the previous (plain) value.
pub fn unmask(incoming: &mut Map<String, Value>, previous: Option<&Map<String, Value>>) {
    for (k, v) in incoming.iter_mut() {
        if v.as_str() == Some(SECRET_MASK) {
            *v = previous
                .and_then(|p| p.get(k))
                .cloned()
                .unwrap_or_else(|| Value::String(String::new()));
        }
    }
}

/// Same as [`unmask`] for a single optional string.
pub fn unmask_opt(incoming: Option<String>, previous: Option<&String>) -> Option<String> {
    match incoming {
        Some(s) if s == SECRET_MASK => previous.cloned(),
        other => other,
    }
}

/// Masks a single optional string.
pub fn mask_opt(v: &Option<String>) -> Option<String> {
    v.as_ref().map(|s| {
        if s.is_empty() {
            String::new()
        } else {
            SECRET_MASK.to_owned()
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn encrypt_mask_unmask_roundtrip() {
        let sb = SecretBox::from_key([3; 32]);
        let plain = json!({"api_key": "k1", "host": "h", "empty": ""})
            .as_object()
            .unwrap()
            .clone();
        let enc = encrypt_fields(&sb, &plain, ["api_key", "empty"]).unwrap();
        assert!(enc["api_key"].as_str().unwrap().starts_with(ENC_PREFIX));
        assert_eq!(enc["empty"], "");
        assert_eq!(enc["host"], "h");
        let mut dec = enc.clone();
        decrypt_all(&sb, &mut dec).unwrap();
        assert_eq!(dec, plain);

        let masked = mask(&dec, ["api_key", "empty"]);
        assert_eq!(masked["api_key"], SECRET_MASK);
        assert_eq!(masked["empty"], "");
        let mut back = masked.clone();
        unmask(&mut back, Some(&dec));
        assert_eq!(back, plain);

        assert_eq!(
            unmask_opt(Some(SECRET_MASK.into()), Some(&"x".into())),
            Some("x".into())
        );
        assert_eq!(
            unmask_opt(Some("new".into()), Some(&"x".into())),
            Some("new".into())
        );
        assert_eq!(mask_opt(&Some("x".into())), Some(SECRET_MASK.into()));
    }

    #[test]
    fn wrong_key_is_reported() {
        let a = SecretBox::from_key([1; 32]);
        let b = SecretBox::from_key([2; 32]);
        let enc = encrypt_fields(&a, json!({"k": "v"}).as_object().unwrap(), ["k"]).unwrap();
        let mut m = enc;
        assert!(decrypt_all(&b, &mut m).is_err());
    }
}
