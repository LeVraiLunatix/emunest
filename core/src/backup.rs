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

/// Dossiers de sauvegardes de l'émulateur qui existent réellement : en mode portable (`portable.txt`
/// à côté de l'exécutable) ceux du dossier de l'exécutable, sinon les emplacements habituels de l'utilisateur.
pub fn save_dirs(exe: &Path, profile: &Profile) -> Vec<PathBuf> {
    let base = exe.parent().unwrap_or(Path::new("."));
    let mut dirs: Vec<PathBuf> = if base.join("portable.txt").exists() || profile.alt_save_dirs.is_empty() {
        profile.save_dirs.iter().map(|d| base.join(d)).collect()
    } else {
        profile.alt_save_dirs.iter().map(|d| expand(d)).collect()
    };
    // un émulateur « portable » sans portable.txt garde parfois ses données à côté de l'exe : on regarde aussi
    dirs.extend(profile.save_dirs.iter().map(|d| base.join(d)));
    dirs.retain(|d| d.is_dir());
    dirs.sort();
    dirs.dedup();
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
    for (i, d) in dirs.iter().enumerate() {
        // un sous-dossier par emplacement (numéroté) : pas de collision entre deux dossiers du même nom
        total += add_dir(&mut zip, d, &format!("{}/{}-{}", profile.name, i, d.file_name().and_then(|n| n.to_str()).unwrap_or("saves")))?;
    }
    zip.finish()?;
    if total == 0 {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    prune(dest_dir, &prefix);
    Ok(Some(path))
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
