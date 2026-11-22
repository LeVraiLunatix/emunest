//! Lancement d'un jeu avec son émulateur.
//! L'exécutable est cherché d'abord dans `config.toml` (section [emulators], nom = chemin),
//! puis dans le dossier géré `<data>/emulators/<nom en minuscules>/` (là où l'installeur le mettra).
use anyhow::{bail, Context, Result};
use emunest_emulators::{Profile, Requirement};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use walkdir::WalkDir;

#[derive(Debug, Default, Deserialize)]
struct Config {
    /// dossier de ROMs par défaut (sinon : argument de la ligne de commande)
    roms: Option<PathBuf>,
    #[serde(default)]
    emulators: HashMap<String, PathBuf>,
}

fn read_config(data: &Path) -> Config {
    std::fs::read_to_string(data.join("config.toml"))
        .ok()
        .and_then(|s| toml::from_str(&s).map_err(|e| tracing::warn!("config.toml : {e}")).ok())
        .unwrap_or_default()
}

/// Dossier de ROMs indiqué dans config.toml (`roms = 'D:\\Jeux'`).
pub fn roms_dir(data: &Path) -> Option<PathBuf> {
    read_config(data).roms
}

/// Dossier où EmuNest range les émulateurs qu'il gère.
pub fn managed_dir(data: &Path, profile: &Profile) -> PathBuf {
    data.join("emulators").join(profile.name.to_lowercase())
}

/// Chemin de l'exécutable de l'émulateur, s'il est installé.
pub fn find_executable(data: &Path, profile: &Profile) -> Option<PathBuf> {
    let cfg = read_config(data);
    if let Some(p) = cfg.emulators.get(&profile.name).filter(|p| p.is_file()) {
        return Some(p.clone());
    }
    find_in(&managed_dir(data, profile), &profile.executable)
}

/// Cherche `name` dans `dir` : les archives s'extraient souvent dans un sous-dossier, profondeur limitée à 3.
pub(crate) fn find_in(dir: &Path, name: &str) -> Option<PathBuf> {
    WalkDir::new(dir)
        .max_depth(3)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.file_type().is_file() && e.file_name().eq_ignore_ascii_case(name))
        .map(|e| e.into_path())
}

/// Remplace les %VARIABLES% d'environnement d'un chemin.
fn expand(path: &str) -> PathBuf {
    let mut out = String::new();
    let mut parts = path.split('%');
    out.push_str(parts.next().unwrap_or_default());
    while let (Some(var), Some(rest)) = (parts.next(), parts.next()) {
        out.push_str(&std::env::var(var).unwrap_or_default());
        out.push_str(rest);
    }
    PathBuf::from(out)
}

/// Dossiers où l'émulateur cherche ce fichier, le plus probable en premier :
/// en mode portable (`portable.txt`) à côté de l'exécutable, sinon les dossiers habituels de l'utilisateur.
pub fn requirement_dirs(exe: &Path, req: &Requirement) -> Vec<PathBuf> {
    let base = exe.parent().unwrap_or(Path::new("."));
    let local = base.join(&req.dir);
    if base.join("portable.txt").exists() {
        return vec![local];
    }
    let mut dirs: Vec<PathBuf> = req.alt_dirs.iter().map(|d| expand(d)).collect();
    dirs.push(local);
    dirs
}

/// Dossier où dire à l'utilisateur de poser le fichier.
pub fn requirement_dir(exe: &Path, req: &Requirement) -> PathBuf {
    requirement_dirs(exe, req).remove(0)
}

/// Fichiers exigés par le profil qui ne sont pas (encore) là.
pub fn missing_files<'a>(exe: &Path, profile: &'a Profile) -> Vec<&'a Requirement> {
    profile
        .requires
        .iter()
        .filter(|req| {
            let found = requirement_dirs(exe, req).iter().any(|dir| {
                std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok()).any(|e| {
                    let path = e.path();
                    let ext_ok = path
                        .extension()
                        .and_then(|x| x.to_str())
                        .is_some_and(|x| req.extensions.iter().any(|w| w.eq_ignore_ascii_case(x)));
                    ext_ok && e.metadata().is_ok_and(|m| m.is_file() && m.len() >= req.min_kb * 1024)
                })
            });
            !found
        })
        .collect()
}

/// Démarre l'émulateur sur `rom` (`{rom}` est remplacé dans les arguments du profil).
pub fn launch(exe: &Path, profile: &Profile, rom: &Path) -> Result<Child> {
    if !rom.is_file() {
        bail!("le fichier {} n'existe plus", rom.display());
    }
    let rom = rom.to_string_lossy();
    let args = profile.launch_args.iter().map(|a| a.replace("{rom}", &rom));
    Command::new(exe)
        .args(args)
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .spawn()
        .with_context(|| format!("impossible de lancer {}", exe.display()))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use emunest_emulators::Source;

    #[test]
    fn rom_placeholder_is_substituted() {
        let dir = std::env::temp_dir().join(format!("emunest-launch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rom = dir.join("game with space.iso");
        std::fs::write(&rom, b"x").unwrap();

        // cmd sort avec le code 7 seulement si {rom} a bien été remplacé par un fichier existant
        let profile = Profile {
            name: "Fake".into(),
            platforms: vec!["gc".into()],
            extensions: vec!["iso".into()],
            source: Source::Url { url: String::new() },
            executable: "cmd.exe".into(),
            launch_args: ["/C", "if", "exist", "{rom}", "(exit", "7)"].map(String::from).to_vec(),
            portable: false,
            requires: vec![],
        };
        let exe = PathBuf::from(std::env::var("COMSPEC").unwrap());
        let status = launch(&exe, &profile, &rom).unwrap().wait().unwrap();
        assert_eq!(status.code(), Some(7));
        assert!(launch(&exe, &profile, &dir.join("missing.iso")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_files_follow_the_folder_content() {
        let dir = std::env::temp_dir().join(format!("emunest-req-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bios")).unwrap();
        std::fs::write(dir.join("portable.txt"), b"").unwrap(); // mode portable : seul le dossier de l'exe compte
        let exe = dir.join("emu.exe");
        let profile = emunest_emulators::builtin_profiles().unwrap().into_iter().find(|p| p.name == "PCSX2").unwrap();

        assert_eq!(missing_files(&exe, &profile).len(), 1); // dossier vide
        std::fs::write(dir.join("bios").join("tiny.bin"), vec![0u8; 1024]).unwrap();
        assert_eq!(missing_files(&exe, &profile).len(), 1); // trop petit
        std::fs::write(dir.join("bios").join("scph.BIN"), vec![0u8; 4 * 1024 * 1024]).unwrap();
        assert!(missing_files(&exe, &profile).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
