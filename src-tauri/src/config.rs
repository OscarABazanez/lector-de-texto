//! Configuración persistente en `%APPDATA%\lector\config.toml`.

use std::path::PathBuf;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::lang::Lang;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// Voces del sistema: WinRT en Windows, `say` en macOS.
    #[default]
    #[serde(alias = "windows")]
    System,
    Edge,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hotkey: String,
    pub llm: LlmConfig,
    pub tts: TtsConfig,
    pub ui: UiConfig,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    /// Modelo de Claude; la API key se guarda aparte (ver `crate::secrets`).
    pub model: String,
    /// Razonamiento adaptativo: el modelo decide cuánto pensar. Apagado responde más rápido.
    pub adaptive: bool,
    /// Deja razonar al modelo: más exacto con fórmulas difíciles, pero más lento.
    pub precise_mode: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TtsConfig {
    pub engine: Engine,
    /// Multiplicador de velocidad (1.0 = normal).
    pub rate: f32,
    /// Voz del sistema; vacía = la predeterminada del sistema para ese idioma.
    #[serde(alias = "windows_es")]
    pub system_es: String,
    #[serde(alias = "windows_en")]
    pub system_en: String,
    pub edge_es: String,
    pub edge_en: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub default_language: Lang,
    pub always_on_top: bool,
    /// Resaltar el fragmento de la fórmula que se está leyendo (las largas tardan ~1 s más).
    pub math_parts: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "Ctrl+Alt+L".into(),
            llm: LlmConfig::default(),
            tts: TtsConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model: "claude-haiku-5-5".into(),
            adaptive: true,
            precise_mode: false,
        }
    }
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            engine: Engine::System,
            rate: 1.0,
            // En macOS se deja vacía para usar la voz que el usuario eligió en el sistema.
            system_es: if cfg!(windows) { "Microsoft Sabina" } else { "" }.into(),
            system_en: if cfg!(windows) { "Microsoft Zira" } else { "" }.into(),
            edge_es: "es-MX-DaliaNeural".into(),
            edge_en: "en-US-AriaNeural".into(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            default_language: Lang::Es,
            always_on_top: true,
            math_parts: true,
        }
    }
}

impl TtsConfig {
    pub fn voice(&self, engine: Engine, lang: Lang) -> &str {
        match (engine, lang) {
            (Engine::System, Lang::Es) => &self.system_es,
            (Engine::System, Lang::En) => &self.system_en,
            (Engine::Edge, Lang::Es) => &self.edge_es,
            (Engine::Edge, Lang::En) => &self.edge_en,
        }
    }
}

pub fn path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.config_dir().join("lector").join("config.toml"))
}

impl Config {
    /// Lee la configuración; si no existe la crea con los valores por defecto.
    pub fn load() -> Config {
        let Some(path) = path() else {
            return Config::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut cfg: Config = toml::from_str(&text).unwrap_or_else(|e| {
                    eprintln!("config inválida en {}: {e}; usando valores por defecto", path.display());
                    Config::default()
                });
                if cfg.migrate() {
                    if let Err(e) = cfg.save() {
                        eprintln!("no se pudo actualizar {}: {e:#}", path.display());
                    }
                }
                cfg
            }
            Err(_) => {
                let cfg = Config::default();
                if let Err(e) = cfg.save() {
                    eprintln!("no se pudo crear {}: {e:#}", path.display());
                }
                cfg
            }
        }
    }

    /// Las versiones anteriores usaban un modelo local de LM Studio. Devuelve si cambió algo.
    pub fn migrate(&mut self) -> bool {
        if self.llm.model.starts_with("claude-") {
            return false;
        }
        self.llm.model = LlmConfig::default().model;
        true
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = path().context("no se encontró la carpeta de configuración")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migra_config_de_lm_studio() {
        let old = "hotkey = \"Ctrl+Alt+K\"\n[llm]\nbase_url = \"http://localhost:1234/v1\"\nmodel = \"google/gemma-4-e4b\"\nprecise_mode = true\n";
        let mut cfg: Config = toml::from_str(old).unwrap();
        assert!(cfg.migrate());
        assert_eq!(cfg.llm.model, "claude-haiku-5-5");
        assert!(cfg.llm.precise_mode);
        assert_eq!(cfg.hotkey, "Ctrl+Alt+K");
        assert!(!cfg.migrate());
        assert!(!toml::to_string(&cfg).unwrap().contains("base_url"));
    }

    #[test]
    fn lee_motor_de_windows_de_versiones_anteriores() {
        let old = "[tts]\nengine = \"windows\"\nwindows_es = \"Microsoft Raul\"\n";
        let cfg: Config = toml::from_str(old).unwrap();
        assert_eq!(cfg.tts.engine, Engine::System);
        assert_eq!(cfg.tts.system_es, "Microsoft Raul");
        assert!(toml::to_string(&cfg).unwrap().contains("engine = \"system\""));
    }
}
