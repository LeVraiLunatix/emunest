//! `config.toml` du dossier de données : dossiers de ROMs, émulateurs déclarés à la main, sauvegardes.
use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Accepte `roms = 'D:\Jeux'` (un seul dossier, ancien format) comme `roms = ['D:\Jeux', 'E:\Autres']`.
fn one_or_many<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<PathBuf>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(PathBuf),
        Many(Vec<PathBuf>),
    }
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(p) => vec![p],
        OneOrMany::Many(v) => v,
    })
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    /// dossiers de ROMs scannés (et surveillés)
    #[serde(default, deserialize_with = "one_or_many")]
    pub roms: Vec<PathBuf>,
    /// émulateurs déjà installés ailleurs : nom du profil -> chemin de l'exécutable
    #[serde(default)]
    pub emulators: BTreeMap<String, PathBuf>,
    /// dossier des sauvegardes de jeu (zip) ; par défaut `<données>/backups`
    pub backup_dir: Option<PathBuf>,
    /// sauvegarde automatique des saves après chaque partie
    #[serde(default)]
    pub auto_backup: bool,
    /// jeton GitHub (facultatif) pour chercher les mises à jour d'EmuNest dans un dépôt privé
    pub github_token: Option<String>,
}

pub fn load(data: &Path) -> Config {
    std::fs::read_to_string(data.join("config.toml"))
        .ok()
        .and_then(|s| toml::from_str(&s).map_err(|e| tracing::warn!("config.toml : {e}")).ok())
        .unwrap_or_default()
}

pub fn save(data: &Path, cfg: &Config) -> Result<()> {
    std::fs::create_dir_all(data)?;
    let body = toml::to_string_pretty(cfg)?;
    std::fs::write(
        data.join("config.toml"),
        format!("# Configuration d'EmuNest (modifiable à la main ou depuis Réglages)\n{body}"),
    )?;
    Ok(())
}

impl Config {
    pub fn backup_dir(&self, data: &Path) -> PathBuf {
        self.backup_dir.clone().unwrap_or_else(|| data.join("backups"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roms_accepts_one_path_or_a_list_and_round_trips() {
        let one: Config = toml::from_str("roms = 'D:\\Jeux'\n[emulators]\nPCSX2 = 'D:\\p.exe'").unwrap();
        assert_eq!(one.roms, vec![PathBuf::from("D:\\Jeux")]);
        assert_eq!(one.emulators["PCSX2"], PathBuf::from("D:\\p.exe"));
        let many: Config = toml::from_str("roms = ['A:\\x', 'B:\\y']").unwrap();
        assert_eq!(many.roms.len(), 2);
        let again: Config = toml::from_str(&toml::to_string_pretty(&many).unwrap()).unwrap();
        assert_eq!(again.roms, many.roms);
    }
}
