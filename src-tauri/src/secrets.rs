//! API key de Anthropic: en el almacén de credenciales del sistema, nunca en config.toml.

use anyhow::Result;
use serde::Serialize;

const SERVICE: &str = "lector";
const USER: &str = "anthropic_api_key";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeySource {
    /// Administrador de credenciales de Windows (Keychain en macOS).
    Keyring,
    /// Variable de entorno `ANTHROPIC_API_KEY`.
    Env,
}

/// La key guardada por la app o, si no hay, la de la variable de entorno.
pub fn get_api_key() -> Option<(String, KeySource)> {
    let stored = keyring::Entry::new(SERVICE, USER)
        .and_then(|e| e.get_password())
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty());
    if let Some(k) = stored {
        return Some((k, KeySource::Keyring));
    }
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
        .map(|k| (k, KeySource::Env))
}

pub fn set_api_key(key: &str) -> Result<()> {
    keyring::Entry::new(SERVICE, USER)?.set_password(key.trim())?;
    Ok(())
}

pub fn clear_api_key() -> Result<()> {
    match keyring::Entry::new(SERVICE, USER)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
