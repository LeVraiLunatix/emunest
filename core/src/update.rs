//! Mise à jour d'EmuNest lui-même : cherche la dernière release GitHub, télécharge le zip et remplace l'exécutable.
//! Un dépôt privé demande un jeton (`github_token` dans config.toml) ; sans jeton la vérification échoue en silence.
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REPO: &str = "LeVraiLunatix/emunest";

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    /// URL de l'API pour télécharger le zip (fonctionne aussi pour un dépôt privé avec un jeton)
    pub asset_url: String,
    pub asset_name: String,
}

/// « v1.2.3 » / « 1.2.3-beta » -> (1, 2, 3)
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut it = core.split('.').map(|n| n.parse::<u64>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

/// Vrai si `latest` est strictement plus récente que `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

/// Jeton de l'utilisateur déjà connecté à GitHub avec `gh` (jamais enregistré, lu à la demande) :
/// permet de suivre les releases d'un dépôt privé sans rien configurer.
pub fn gh_token() -> Option<String> {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("gh").args(["auth", "token"]).creation_flags(0x0800_0000).output().ok()?; // CREATE_NO_WINDOW
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !token.is_empty()).then_some(token)
}

/// Vrai pour un exécutable compilé sur place (`…\target\release\…`) : on ne le remplace jamais automatiquement.
pub fn is_dev_build(exe: &Path) -> bool {
    let parts: Vec<_> = exe.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect();
    parts.windows(2).any(|w| w[0] == "target" && (w[1] == "release" || w[1] == "debug"))
}

fn client(token: Option<&str>) -> Result<reqwest::blocking::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        headers.insert(reqwest::header::AUTHORIZATION, format!("Bearer {t}").parse()?);
    }
    Ok(reqwest::blocking::Client::builder()
        .user_agent("emunest")
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(15))
        .timeout(None::<Duration>)
        .build()?)
}

/// La dernière release si elle est plus récente que `current`.
pub fn check(current: &str, token: Option<&str>) -> Result<Option<Release>> {
    let resp = client(token)?.get(format!("https://api.github.com/repos/{REPO}/releases/latest")).send()?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        bail!("aucune release accessible (dépôt privé sans jeton, ou pas encore de release)");
    }
    let json: serde_json::Value = serde_json::from_slice(&resp.error_for_status()?.bytes()?)?;
    let version = json["tag_name"].as_str().context("release sans étiquette")?.trim_start_matches('v').to_string();
    if !is_newer(&version, current) {
        return Ok(None);
    }
    let asset = json["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"].as_str().is_some_and(|n| n.ends_with("windows-x64.zip")))
        .context("pas d'archive Windows dans la release")?;
    Ok(Some(Release {
        version,
        asset_url: asset["url"].as_str().context("asset sans URL")?.to_string(),
        asset_name: asset["name"].as_str().unwrap_or("update.zip").to_string(),
    }))
}

/// Remplace `exe` par `new` : un exécutable en cours d'exécution peut être renommé sous Windows, pas écrasé.
/// L'ancien reste à côté (`.old`) jusqu'au prochain démarrage.
pub fn swap_exe(exe: &Path, new: &Path) -> Result<()> {
    let old = exe.with_extension("old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).context("l'exécutable actuel ne peut pas être renommé")?;
    if let Err(e) = std::fs::copy(new, exe) {
        let _ = std::fs::rename(&old, exe); // on remet l'ancien : jamais d'EmuNest cassé
        return Err(e).context("copie de la nouvelle version");
    }
    Ok(())
}

/// Télécharge la release, en extrait `emunest.exe` et remplace l'exécutable courant. Renvoie son chemin.
pub fn apply(release: &Release, token: Option<&str>, progress: &mut dyn FnMut(f32)) -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let work = std::env::temp_dir().join(format!("emunest-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;

    let mut resp = client(token)?
        .get(&release.asset_url)
        .header(reqwest::header::ACCEPT, "application/octet-stream")
        .send()?
        .error_for_status()?;
    let total = resp.content_length();
    let zip_path = work.join(&release.asset_name);
    let mut out = std::fs::File::create(&zip_path)?;
    let (mut done, mut buf) = (0u64, vec![0u8; 64 * 1024]);
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        if let Some(t) = total {
            progress(done as f32 / t as f32);
        }
    }
    drop(out);

    zip::ZipArchive::new(std::fs::File::open(&zip_path)?)?.extract(&work)?;
    let new_exe = walkdir::WalkDir::new(&work)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().eq_ignore_ascii_case("emunest.exe"))
        .context("emunest.exe absent de l'archive")?
        .into_path();
    swap_exe(&exe, &new_exe)?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_builds_are_never_auto_updated() {
        assert!(is_dev_build(Path::new(r"D:\Code\emulaunch\target\release\emunest.exe")));
        assert!(is_dev_build(Path::new(r"D:\x\Target\Debug\emunest.exe")));
        assert!(!is_dev_build(Path::new(r"C:\Users\a\AppData\Local\Programs\EmuNest\emunest.exe")));
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("n'importe quoi", "0.1.0"));
    }

    #[test]
    fn swap_keeps_the_old_exe_aside_and_restores_on_failure() {
        let dir = std::env::temp_dir().join(format!("emunest-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (exe, new) = (dir.join("emunest.exe"), dir.join("new.exe"));
        std::fs::write(&exe, b"ancienne").unwrap();
        std::fs::write(&new, b"nouvelle").unwrap();
        swap_exe(&exe, &new).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"nouvelle");
        assert_eq!(std::fs::read(exe.with_extension("old")).unwrap(), b"ancienne");

        // nouvelle version introuvable : l'ancienne doit revenir
        assert!(swap_exe(&exe, &dir.join("absent.exe")).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"nouvelle");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
