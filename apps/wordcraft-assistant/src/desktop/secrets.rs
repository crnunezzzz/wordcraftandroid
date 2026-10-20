//! API keys at rest. Windows keeps them in the Credential Manager and macOS in the Keychain
//! (encrypted for the signed-in user); elsewhere a file only the user can read. They are read
//! only to send a request and never written to the settings or logs.
#![forbid(unsafe_code)]

use crate::protocol::Provider;
use serde_json::{Value, json};
use std::path::PathBuf;

pub struct Secrets {
    /// The credential's service name ("WordCraft assistant").
    #[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
    service: String,
    /// The settings folder (the key file where there is no system store).
    #[cfg_attr(any(target_os = "windows", target_os = "macos"), allow(dead_code))]
    dir: PathBuf,
}

impl Secrets {
    pub fn new(app: &str, dir: PathBuf) -> Self {
        Self { service: format!("{app} assistant"), dir }
    }

    /// Which providers have a key: `{"openai":bool,"anthropic":bool}`.
    pub fn flags(&self) -> Value {
        json!({"openai": self.get(Provider::OpenAi).is_some(), "anthropic": self.get(Provider::Anthropic).is_some()})
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    fn entry(&self, provider: Provider) -> Result<keyring::Entry, String> {
        keyring::Entry::new(&self.service, provider.id()).map_err(|e| e.to_string())
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub fn get(&self, provider: Provider) -> Option<String> {
        self.entry(provider).ok()?.get_password().ok().filter(|k| !k.is_empty())
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub fn set(&self, provider: Provider, key: &str) -> Result<(), String> {
        self.entry(provider)?.set_password(key).map_err(|e| format!("No se pudo guardar la clave: {e}"))
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub fn delete(&self, provider: Provider) -> Result<(), String> {
        match self.entry(provider)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("No se pudo eliminar la clave: {e}")),
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn file(&self) -> PathBuf {
        self.dir.join("assistant-keys.json")
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn all(&self) -> serde_json::Map<String, Value> {
        std::fs::read(self.file())
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn write_all(&self, keys: &serde_json::Map<String, Value>) -> Result<(), String> {
        use std::io::Write;
        let tmp = self.dir.join("assistant-keys.json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
        file.write_all(Value::Object(keys.clone()).to_string().as_bytes()).map_err(|e| e.to_string())?;
        drop(file);
        std::fs::rename(&tmp, self.file()).map_err(|e| e.to_string())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    pub fn get(&self, provider: Provider) -> Option<String> {
        self.all().get(provider.id())?.as_str().filter(|k| !k.is_empty()).map(str::to_owned)
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    pub fn set(&self, provider: Provider, key: &str) -> Result<(), String> {
        let mut keys = self.all();
        keys.insert(provider.id().into(), json!(key));
        self.write_all(&keys)
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    pub fn delete(&self, provider: Provider) -> Result<(), String> {
        let mut keys = self.all();
        keys.remove(provider.id());
        self.write_all(&keys)
    }
}
