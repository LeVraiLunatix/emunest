//! Captures d'écran par jeu : pendant une partie, on repère les nouvelles images dans les dossiers de
//! captures de l'émulateur ; elles sont ensuite copiées dans la galerie du jeu.
use crate::backup::candidate_dirs;
use emunest_emulators::Profile;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;

/// Extensions d'images que la galerie sait afficher.
pub fn is_image(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["png", "jpg", "jpeg", "bmp"].iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// À créer avant de lancer l'émulateur ; `new_shots` donne les images apparues depuis.
pub struct ShotWatch {
    dirs: Vec<PathBuf>,
    start: SystemTime,
}

impl ShotWatch {
    pub fn new(exe: &Path, profile: &Profile) -> Self {
        // le dossier de captures peut ne pas exister encore : il est créé par l'émulateur pendant la partie
        Self {
            dirs: candidate_dirs(exe, &profile.screenshot_dirs, &profile.alt_screenshot_dirs),
            // marge de quelques secondes : horloge du système de fichiers et lancement de l'émulateur
            start: SystemTime::now() - Duration::from_secs(2),
        }
    }

    pub fn new_shots(&self) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = self
            .dirs
            .iter()
            .filter(|d| d.is_dir())
            .flat_map(|d| WalkDir::new(d).max_depth(3).into_iter().filter_map(|e| e.ok()))
            .filter(|e| e.file_type().is_file() && is_image(e.path()))
            .filter(|e| e.metadata().ok().and_then(|m| m.modified().ok()).is_some_and(|t| t >= self.start))
            .map(|e| e.into_path())
            .collect();
        found.sort();
        found.dedup();
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_only_images_created_during_the_session() {
        let root = std::env::temp_dir().join(format!("emunest-shots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let emu = root.join("emu");
        std::fs::create_dir_all(emu.join("snaps")).unwrap();
        std::fs::write(emu.join("portable.txt"), b"").unwrap();
        // une vieille capture (avant la partie)
        let old = emu.join("snaps/old.png");
        std::fs::write(&old, b"x").unwrap();
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(SystemTime::now() - Duration::from_secs(3600)).unwrap();

        let profile = emunest_emulators::builtin_profiles().unwrap().into_iter().find(|p| p.name == "PCSX2").unwrap();
        let watch = ShotWatch::new(&emu.join("pcsx2-qt.exe"), &profile);
        std::fs::write(emu.join("snaps/new.png"), b"x").unwrap();
        std::fs::write(emu.join("snaps/notes.txt"), b"x").unwrap();

        let names: Vec<String> = watch.new_shots().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["new.png".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
