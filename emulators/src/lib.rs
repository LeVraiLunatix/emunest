//! Profils d'émulateurs : un fichier TOML par émulateur, embarqués dans le binaire.
use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Source {
    Github { repo: String },
    /// Forge Forgejo / Gitea (API compatible GitHub) : `host` + `repo` (« projects/Ryubing »).
    Forge { host: String, repo: String },
    Url { url: String },
    /// Pas d'installation automatique : l'utilisateur indique le chemin dans config.toml.
    Manual,
    /// Page web dont on extrait le premier lien (https://…) se terminant par `ends_with`.
    Page { url: String, ends_with: String },
}

/// Fichier que l'utilisateur doit fournir lui-même (BIOS, clés…) : EmuNest ne le télécharge jamais.
#[derive(Debug, Clone, Deserialize)]
pub struct Requirement {
    /// Nom affiché (« BIOS PlayStation 2 »)
    pub label: String,
    /// Dossier attendu, relatif au dossier de l'exécutable de l'émulateur
    pub dir: String,
    /// Extensions acceptées, sans le point
    pub extensions: Vec<String>,
    /// Autres dossiers possibles quand l'émulateur n'est pas en mode portable (variables %NOM% acceptées)
    #[serde(default)]
    pub alt_dirs: Vec<String>,
    /// Taille minimale en Ko (écarte les fichiers vides ou tronqués)
    #[serde(default)]
    pub min_kb: u64,
    /// Aide affichée à l'utilisateur
    #[serde(default)]
    pub hint: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub name: String,
    pub platforms: Vec<String>,
    pub extensions: Vec<String>,
    pub source: Source,
    pub executable: String,
    pub launch_args: Vec<String>,
    /// Crée `portable.txt` à l'installation : l'émulateur range alors tout dans son dossier
    #[serde(default)]
    pub portable: bool,
    #[serde(default)]
    pub requires: Vec<Requirement>,
    /// dossiers de sauvegardes (relatifs au dossier de l'exécutable) à sauvegarder en mode portable
    #[serde(default)]
    pub save_dirs: Vec<String>,
    /// dossiers de sauvegardes hors mode portable (variables %NOM% acceptées)
    #[serde(default)]
    pub alt_save_dirs: Vec<String>,
    /// dossiers où l'émulateur range ses captures d'écran (mêmes règles que `save_dirs`)
    #[serde(default)]
    pub screenshot_dirs: Vec<String>,
    #[serde(default)]
    pub alt_screenshot_dirs: Vec<String>,
}

const BUILTIN: &[&str] = &[
    include_str!("../profiles/dolphin.toml"),
    include_str!("../profiles/pcsx2.toml"),
    include_str!("../profiles/duckstation.toml"),
    include_str!("../profiles/ppsspp.toml"),
    include_str!("../profiles/mgba.toml"),
    include_str!("../profiles/ryujinx.toml"),
    include_str!("../profiles/cemu.toml"),
    include_str!("../profiles/rpcs3.toml"),
    include_str!("../profiles/melonds.toml"),
    include_str!("../profiles/azahar.toml"),
];

pub fn builtin_profiles() -> Result<Vec<Profile>> {
    BUILTIN
        .iter()
        .map(|s| toml::from_str(s).context("profil d'émulateur invalide"))
        .collect()
}

/// Nom lisible d'une plateforme (l'id lui-même si inconnue).
pub fn platform_name(id: &str) -> &str {
    match id {
        "gc" => "GameCube",
        "wii" => "Wii",
        "ps2" => "PlayStation 2",
        "psx" => "PlayStation",
        "psp" => "PSP",
        "switch" => "Nintendo Switch",
        "wiiu" => "Wii U",
        "ps3" => "PlayStation 3",
        "nds" => "Nintendo DS",
        "3ds" => "Nintendo 3DS",
        "gba" => "Game Boy Advance",
        "gbc" => "Game Boy Color",
        "gb" => "Game Boy",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn profiles_parse() {
        assert_eq!(super::builtin_profiles().unwrap().len(), 10);
    }
}

/// Proportions (largeur / hauteur) du boîtier de la console : sert à la forme des jaquettes
/// quand on n'a pas la vraie image (sinon on prend celle de l'image).
pub fn cover_aspect(platform: &str) -> f32 {
    match platform {
        "ps2" | "wii" | "wiiu" => 0.71, // boîtier DVD
        "ps3" => 0.69,                  // boîtier Blu-ray
        "nds" => 0.9,
        "3ds" => 0.88,
        "psx" => 0.96,         // boîtier CD
        "psp" => 0.58,         // boîtier UMD
        "gc" => 0.76,          // boîtier mini-DVD
        "gba" | "gbc" | "gb" => 0.9,
        "switch" => 0.62,      // boîtier de cartouche
        _ => 0.71,
    }
}

/// Nom du dépôt libretro-thumbnails pour une plateforme (pochettes), si on en connaît un.
pub fn libretro_system(id: &str) -> Option<&'static str> {
    Some(match id {
        "gc" => "Nintendo - GameCube",
        "wii" => "Nintendo - Wii",
        "ps2" => "Sony - PlayStation 2",
        "psx" => "Sony - PlayStation",
        "psp" => "Sony - PlayStation Portable",
        "gba" => "Nintendo - Game Boy Advance",
        "gbc" => "Nintendo - Game Boy Color",
        "gb" => "Nintendo - Game Boy",
        "wiiu" => "Nintendo - Wii U",
        "ps3" => "Sony - PlayStation 3",
        "nds" => "Nintendo - Nintendo DS",
        "3ds" => "Nintendo - Nintendo 3DS",
        _ => return None,
    })
}
