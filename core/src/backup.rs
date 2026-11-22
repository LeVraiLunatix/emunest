//! Sauvegarde des fichiers de sauvegarde (saves, memory cards, états) des émulateurs dans des zip datés.
//! À pointer vers un dossier synchronisé (OneDrive, Dropbox…) pour avoir une copie dans le cloud.
use crate::launch::{expand, find_executable};
use anyhow::{Context, Result};
use emunest_emulators::Profile;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use walkdir::WalkDir;

/// Nombre de sauvegardes conservées par émulateur.
const KEEP: usize = 10;
/// Fichier du zip qui relie chaque dossier sauvegardé à son emplacement d'origine.
const MANIFEST: &str = "manifest.txt";

/// Emplacements possibles (existants ou non) d'une liste de dossiers de l'émulateur : en mode portable
/// (`portable.txt` à côté de l'exécutable) ceux du dossier de l'exécutable, sinon les dossiers habituels de l'utilisateur.
pub fn candidate_dirs(exe: &Path, rel: &[String], alt: &[String]) -> Vec<PathBuf> {
    let base = exe.parent().unwrap_or(Path::new("."));
    let mut dirs: Vec<PathBuf> = if base.join("portable.txt").exists() || alt.is_empty() {
        rel.iter().map(|d| base.join(d)).collect()
    } else {
        alt.iter().map(|d| expand(d)).collect()
    };
    // un émulateur « portable » sans portable.txt garde parfois ses données à côté de l'exe : on regarde aussi
    dirs.extend(rel.iter().map(|d| base.join(d)));
    dirs.sort();
    dirs.dedup();
    dirs
}

/// Dossiers de sauvegardes de l'émulateur qui existent réellement.
pub fn save_dirs(exe: &Path, profile: &Profile) -> Vec<PathBuf> {
    let mut dirs = candidate_dirs(exe, &profile.save_dirs, &profile.alt_save_dirs);
    dirs.retain(|d| d.is_dir());
    dirs
}

/// `20261122-143005` (UTC) depuis des secondes Unix, sans dépendance de calendrier.
fn timestamp(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // jours -> date civile (algorithme de Howard Hinnant)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let (y, doy) = (yoe + era * 400, doe - (365 * yoe + yoe / 4 - yoe / 100));
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, (rem % 3600) / 60, rem % 60)
}

fn now_stamp() -> String {
    timestamp(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()))
}

/// Ajoute un dossier au zip sous `prefix/…`. Renvoie le nombre de fichiers copiés.
fn add_dir(zip: &mut zip::ZipWriter<std::fs::File>, root: &Path, prefix: &str) -> Result<usize> {
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut n = 0;
    for e in WalkDir::new(root).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
        let rel = e.path().strip_prefix(root).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
        zip.start_file(format!("{prefix}/{rel}"), opts)?;
        let mut f = std::fs::File::open(e.path()).with_context(|| format!("lecture de {}", e.path().display()))?;
        std::io::copy(&mut f, zip)?;
        n += 1;
    }
    Ok(n)
}

/// Ne garde que les `KEEP` zip les plus récents dont le nom commence par `prefix`.
fn prune(dir: &Path, prefix: &str) {
    let mut zips: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(".zip")))
        .collect();
    zips.sort(); // les noms contiennent la date : l'ordre alphabétique est l'ordre chronologique
    let extra = zips.len().saturating_sub(KEEP);
    for old in zips.into_iter().take(extra) {
        let _ = std::fs::remove_file(old);
    }
}

/// Sauvegarde les saves d'un émulateur dans `dest_dir`. Renvoie le zip créé, ou `None` s'il n'y avait rien à copier.
pub fn backup_emulator(exe: &Path, profile: &Profile, dest_dir: &Path) -> Result<Option<PathBuf>> {
    let dirs = save_dirs(exe, profile);
    if dirs.is_empty() {
        return Ok(None);
    }
    std::fs::create_dir_all(dest_dir)?;
    let prefix = format!("EmuNest-{}-", profile.name);
    let path = dest_dir.join(format!("{prefix}{}.zip", now_stamp()));
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path)?);
    let mut total = 0;
    let mut manifest = String::new();
    for (i, d) in dirs.iter().enumerate() {
        // un sous-dossier par emplacement (numéroté) : pas de collision entre deux dossiers du même nom
        let prefix = format!("{}/{}-{}", profile.name, i, d.file_name().and_then(|n| n.to_str()).unwrap_or("saves"));
        total += add_dir(&mut zip, d, &prefix)?;
        manifest.push_str(&format!("{prefix}\t{}\n", d.display())); // d'où vient chaque dossier : sert à la restauration
    }
    zip.start_file(MANIFEST, zip::write::SimpleFileOptions::default())?;
    std::io::Write::write_all(&mut zip, manifest.as_bytes())?;
    zip.finish()?;
    if total == 0 {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    prune(dest_dir, &prefix);
    Ok(Some(path))
}

/// Une sauvegarde trouvée dans le dossier des sauvegardes.
#[derive(Debug, Clone, PartialEq)]
pub struct BackupInfo {
    pub path: PathBuf,
    pub emulator: String,
    /// `20261122-143005` (UTC)
    pub stamp: String,
    pub size: u64,
}

/// Sauvegardes disponibles, les plus récentes d'abord (nom attendu : `EmuNest-<émulateur>-<AAAAMMJJ-HHMMSS>.zip`).
pub fn list_backups(dest_dir: &Path) -> Vec<BackupInfo> {
    let mut v: Vec<BackupInfo> = std::fs::read_dir(dest_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let core = name.strip_prefix("EmuNest-")?.strip_suffix(".zip")?;
            if core.len() < 17 {
                return None;
            }
            let (emu, stamp) = core.split_at(core.len() - 15);
            let emulator = emu.strip_suffix('-')?.to_string();
            let ok = stamp.chars().enumerate().all(|(i, c)| if i == 8 { c == '-' } else { c.is_ascii_digit() });
            ok.then(|| BackupInfo { path: e.path(), emulator, stamp: stamp.to_string(), size: e.metadata().map_or(0, |m| m.len()) })
        })
        .collect();
    v.sort_by(|a, b| b.stamp.cmp(&a.stamp));
    v
}

/// `20261122-143005` -> `22/11/2026 14:30 UTC`
pub fn stamp_label(stamp: &str) -> String {
    if stamp.len() == 15 {
        format!("{}/{}/{} {}:{} UTC", &stamp[6..8], &stamp[4..6], &stamp[..4], &stamp[9..11], &stamp[11..13])
    } else {
        stamp.to_string()
    }
}

/// Restaure une sauvegarde dans les dossiers de saves de l'émulateur. Avant d'écraser quoi que ce soit, les saves
/// actuelles sont elles-mêmes sauvegardées dans `safety_dir` (la restauration est donc réversible).
/// Seuls les dossiers de saves connus du profil sont écrits : un zip ne peut pas écrire ailleurs. Renvoie le nombre de fichiers.
pub fn restore(zip_path: &Path, exe: &Path, profile: &Profile, safety_dir: &Path) -> Result<usize> {
    let allowed = candidate_dirs(exe, &profile.save_dirs, &profile.alt_save_dirs);
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path)?).context("zip illisible")?;

    // préfixe du zip -> dossier d'origine, d'après le manifeste (sinon on retrouve le dossier par son nom)
    let mut origin = std::collections::HashMap::new();
    if let Ok(mut m) = archive.by_name(MANIFEST) {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut m, &mut text)?;
        for line in text.lines() {
            if let Some((prefix, dir)) = line.split_once('\t') {
                origin.insert(prefix.to_string(), PathBuf::from(dir));
            }
        }
    }
    let target_for = |prefix: &str| -> Option<PathBuf> {
        let dir = origin.get(prefix).cloned().or_else(|| {
            let dirname = prefix.rsplit('/').next()?.split_once('-')?.1;
            allowed.iter().find(|a| a.file_name().and_then(|n| n.to_str()) == Some(dirname)).cloned()
        })?;
        allowed.contains(&dir).then_some(dir)
    };

    backup_emulator(exe, profile, safety_dir)?; // filet de sécurité avant d'écraser

    let mut restored = 0;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let Some(rel) = entry.enclosed_name() else { continue }; // refuse « .. » et les chemins absolus
        let parts: Vec<_> = rel.components().collect();
        if parts.len() < 3 || rel == Path::new(MANIFEST) {
            continue;
        }
        let prefix = format!("{}/{}", parts[0].as_os_str().to_string_lossy(), parts[1].as_os_str().to_string_lossy());
        let Some(dir) = target_for(&prefix) else { continue };
        let dest = dir.join(parts[2..].iter().collect::<PathBuf>());
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&dest).with_context(|| format!("écriture de {}", dest.display()))?;
        std::io::copy(&mut entry, &mut out)?;
        restored += 1;
    }
    Ok(restored)
}

/// Sauvegarde tous les émulateurs installés. Renvoie (zip créés, émulateurs sans rien à sauvegarder).
pub fn backup_all(data: &Path, dest_dir: &Path) -> Result<(Vec<PathBuf>, usize)> {
    let (mut made, mut empty) = (Vec::new(), 0);
    for p in emunest_emulators::builtin_profiles()? {
        let Some(exe) = find_executable(data, &p) else { continue };
        match backup_emulator(&exe, &p, dest_dir)? {
            Some(zip) => made.push(zip),
            None => empty += 1,
        }
    }
    Ok((made, empty))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_backups_and_restores_saves_safely() {
        let root = std::env::temp_dir().join(format!("emunest-restore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let emu = root.join("pcsx2");
        std::fs::create_dir_all(emu.join("memcards")).unwrap();
        std::fs::write(emu.join("portable.txt"), b"").unwrap();
        std::fs::write(emu.join("memcards/Mcd001.ps2"), b"AVANT").unwrap();
        let profile = emunest_emulators::builtin_profiles().unwrap().into_iter().find(|p| p.name == "PCSX2").unwrap();
        let exe = emu.join("pcsx2-qt.exe");
        let dest = root.join("backups");
        let zip_path = backup_emulator(&exe, &profile, &dest).unwrap().unwrap();

        let list = list_backups(&dest);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].emulator, "PCSX2");
        assert_eq!(list[0].path, zip_path);
        assert_eq!(stamp_label("20261122-143005"), "22/11/2026 14:30 UTC");

        // la partie suivante abîme la save ; la restauration la remet, après avoir mis l'état actuel à l'abri
        std::fs::write(emu.join("memcards/Mcd001.ps2"), b"CORROMPU").unwrap();
        std::fs::write(emu.join("memcards/Nouvelle.ps2"), b"garde").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100)); // le nom de la sauvegarde de sécurité contient la seconde
        let n = restore(&zip_path, &exe, &profile, &dest).unwrap();
        assert_eq!(n, 1);
        assert_eq!(std::fs::read(emu.join("memcards/Mcd001.ps2")).unwrap(), b"AVANT");
        assert_eq!(std::fs::read(emu.join("memcards/Nouvelle.ps2")).unwrap(), b"garde", "les autres fichiers ne sont pas supprimés");
        assert_eq!(list_backups(&dest).len(), 2, "sauvegarde de sécurité créée avant l'écrasement");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_refuses_paths_outside_the_save_dirs() {
        let root = std::env::temp_dir().join(format!("emunest-zipslip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let emu = root.join("pcsx2");
        std::fs::create_dir_all(emu.join("memcards")).unwrap();
        std::fs::write(emu.join("portable.txt"), b"").unwrap();
        let profile = emunest_emulators::builtin_profiles().unwrap().into_iter().find(|p| p.name == "PCSX2").unwrap();
        let exe = emu.join("pcsx2-qt.exe");

        // zip hostile : entrée « ../ » et manifeste qui désigne un dossier hors des saves
        let evil = root.join("EmuNest-PCSX2-20000101-000000.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&evil).unwrap());
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("PCSX2/0-memcards/../../evil.txt", o).unwrap();
        std::io::Write::write_all(&mut z, b"x").unwrap();
        z.start_file("PCSX2/1-autre/x.txt", o).unwrap();
        std::io::Write::write_all(&mut z, b"x").unwrap();
        z.start_file(MANIFEST, o).unwrap();
        std::io::Write::write_all(&mut z, format!("PCSX2/1-autre\t{}\n", root.join("ailleurs").display()).as_bytes()).unwrap();
        z.finish().unwrap();

        let n = restore(&evil, &exe, &profile, &root.join("backups")).unwrap();
        assert_eq!(n, 0);
        assert!(!root.join("evil.txt").exists() && !emu.join("evil.txt").exists() && !root.join("ailleurs").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn formats_dates_correctly() {
        assert_eq!(timestamp(0), "19700101-000000");
        assert_eq!(timestamp(1_700_000_000), "20231114-221320");
        assert_eq!(timestamp(951_782_400 + 3661), "20000229-010101"); // 29 février d'une année bissextile
    }

    #[test]
    fn backs_up_portable_saves_and_prunes_old_zips() {
        let root = std::env::temp_dir().join(format!("emunest-backup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let emu = root.join("pcsx2");
        std::fs::create_dir_all(emu.join("memcards")).unwrap();
        std::fs::write(emu.join("portable.txt"), b"").unwrap();
        std::fs::write(emu.join("memcards/Mcd001.ps2"), b"save").unwrap();
        let profile = emunest_emulators::builtin_profiles().unwrap().into_iter().find(|p| p.name == "PCSX2").unwrap();
        let exe = emu.join("pcsx2-qt.exe");
        let dest = root.join("backups");

        let zip_path = backup_emulator(&exe, &profile, &dest).unwrap().expect("une sauvegarde");
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
        let names: Vec<String> = (0..archive.len()).map(|i| archive.by_index(i).unwrap().name().unwrap().to_string()).collect();
        assert!(names.iter().any(|n| n.ends_with("memcards/Mcd001.ps2")), "{names:?}");

        // plus de KEEP archives : les plus anciennes disparaissent
        for i in 0..12 {
            std::fs::write(dest.join(format!("EmuNest-PCSX2-2000010{}-0000{:02}.zip", i % 9 + 1, i)), b"x").unwrap();
        }
        prune(&dest, "EmuNest-PCSX2-");
        let left = std::fs::read_dir(&dest).unwrap().count();
        assert_eq!(left, KEEP);
        // sans dossier de saves (et sans les emplacements de l'utilisateur, propres à chaque machine) : rien à faire
        let bare = Profile { alt_save_dirs: vec![], ..profile.clone() };
        assert!(backup_emulator(&root.join("vide/x.exe"), &bare, &dest).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
