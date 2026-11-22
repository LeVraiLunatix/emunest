//! Lancement d'un jeu avec son émulateur.
//! L'exécutable est cherché d'abord dans `config.toml` (section [emulators], nom = chemin),
//! puis dans le dossier géré `<data>/emulators/<nom en minuscules>/` (là où l'installeur le mettra).
use anyhow::{bail, Context, Result};
use emunest_emulators::{Profile, Requirement};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use walkdir::WalkDir;

/// Dossier où EmuNest range les émulateurs qu'il gère.
pub fn managed_dir(data: &Path, profile: &Profile) -> PathBuf {
    data.join("emulators").join(profile.name.to_lowercase())
}

/// Chemin de l'exécutable de l'émulateur, s'il est installé.
pub fn find_executable(data: &Path, profile: &Profile) -> Option<PathBuf> {
    let cfg = crate::config::load(data);
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
pub(crate) fn expand(path: &str) -> PathBuf {
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

/// Découpe une ligne d'arguments sur les espaces ; les guillemets doubles regroupent (`--cfg "mon fichier.ini"`).
pub fn split_args(s: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut any) = (Vec::new(), String::new(), false, false);
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Démarre l'émulateur sur `rom` (`{rom}` est remplacé dans les arguments du profil ; `extra` est ajouté avant la ROM).
pub fn launch(exe: &Path, profile: &Profile, rom: &Path, extra: &str) -> Result<Child> {
    if !rom.is_file() {
        bail!("le fichier {} n'existe plus", rom.display());
    }
    let rom = rom.to_string_lossy();
    // les arguments du profil, avec les arguments du joueur insérés juste avant la ROM (ou ajoutés à la fin)
    let mut args: Vec<String> = profile.launch_args.iter().map(|a| a.replace("{rom}", &rom)).collect();
    let extra = split_args(extra);
    let pos = profile.launch_args.iter().position(|a| a.contains("{rom}")).unwrap_or(args.len());
    // si la ROM suit un « -- », les options du joueur doivent passer avant ce séparateur
    let pos = if pos > 0 && profile.launch_args[pos - 1] == "--" { pos - 1 } else { pos };
    args.splice(pos..pos, extra);
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
            save_dirs: vec![],
            alt_save_dirs: vec![],
        };
        let exe = PathBuf::from(std::env::var("COMSPEC").unwrap());
        let status = launch(&exe, &profile, &rom, "").unwrap().wait().unwrap();
        assert_eq!(status.code(), Some(7));
        assert!(launch(&exe, &profile, &dir.join("missing.iso"), "").is_err());
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

    #[test]
    fn splits_arguments_with_quotes() {
        assert_eq!(split_args(r#"-a --cfg "mon fichier.ini" -b"#), ["-a", "--cfg", "mon fichier.ini", "-b"]);
        assert!(split_args("   ").is_empty());
    }
}
