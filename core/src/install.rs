//! Installation d'un émulateur : trouve la dernière version, la télécharge (avec progression),
//! vérifie son empreinte quand le site la publie, l'extrait dans le dossier géré par EmuNest.
use crate::launch::{find_in, managed_dir};
use anyhow::{bail, Context, Result};
use emunest_emulators::{Profile, Source};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Resolving,
    Downloading,
    Extracting,
}

#[derive(Debug, Clone, PartialEq)]
struct Asset {
    name: String,
    url: String,
    sha256: Option<String>,
}

/// Archive Windows x64 portable parmi les fichiers d'une release (zip/7z, ni installeur, ni symboles, ni ARM).
fn pick_asset(assets: &[Asset]) -> Option<&Asset> {
    assets
        .iter()
        .filter(|a| {
            let n = a.name.to_lowercase();
            n.contains("win")
                && (n.ends_with(".zip") || n.ends_with(".7z"))
                && !["arm", "aarch64", "symbols", "debug", "pdb", "installer", "linux", "mac", "win32", "libretro"].iter().any(|bad| n.contains(bad))
        })
        // préfère le 64 bits, puis le nom le plus court (écarte les variantes « sse2 », « avx2 »…)
        .max_by_key(|a| (a.name.contains("64"), a.name.contains("msvc"), std::cmp::Reverse(a.name.len())))
}

/// Premier lien https:// de la page qui se termine par `ends_with`.
fn first_link(html: &str, ends_with: &str) -> Option<String> {
    html.match_indices("https://")
        .filter_map(|(i, _)| {
            let rest = &html[i..];
            let end = rest.find(|c: char| matches!(c, '"' | '\'' | '<' | '>' | ' ')).unwrap_or(rest.len());
            Some(rest[..end].replace("&amp;", "&"))
        })
        .find(|l| l.ends_with(ends_with))
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent("emunest")
        .connect_timeout(Duration::from_secs(20))
        .timeout(None::<Duration>) // les archives sont grosses : pas de plafond global
        .build()?)
}

fn resolve(http: &reqwest::blocking::Client, source: &Source) -> Result<Asset> {
    match source {
        Source::Github { repo } => {
            let resp = http.get(format!("https://api.github.com/repos/{repo}/releases/latest")).send()?;
            if !resp.status().is_success() {
                bail!("{repo} : aucune release trouvée (HTTP {})", resp.status());
            }
            let json: serde_json::Value = serde_json::from_slice(&resp.bytes()?)?;
            let assets: Vec<Asset> = json["assets"]
                .as_array()
                .context("réponse GitHub inattendue")?
                .iter()
                .filter_map(|a| {
                    Some(Asset {
                        name: a["name"].as_str()?.to_string(),
                        url: a["browser_download_url"].as_str()?.to_string(),
                        sha256: a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_string),
                    })
                })
                .collect();
            pick_asset(&assets).cloned().with_context(|| format!("{repo} : pas d'archive Windows x64 dans la dernière release"))
        }
        Source::Manual => bail!("{} ne s'installe pas automatiquement : installe-le toi-même et indique son chemin dans config.toml", "cet émulateur"),
        Source::Url { url } => Ok(Asset { name: url.rsplit('/').next().unwrap_or("archive").to_string(), url: url.clone(), sha256: None }),
        Source::Page { url, ends_with } => {
            let html = http.get(url).send()?.error_for_status()?.text()?;
            let link = first_link(&html, ends_with).with_context(|| format!("aucun lien « {ends_with} » sur {url}"))?;
            Ok(Asset { name: link.rsplit('/').next().unwrap_or("archive").to_string(), url: link, sha256: None })
        }
    }
}

fn download(http: &reqwest::blocking::Client, asset: &Asset, dest: &Path, progress: &mut dyn FnMut(Phase, f32)) -> Result<()> {
    let mut resp = http.get(&asset.url).send()?.error_for_status()?;
    let total = resp.content_length();
    let mut out = std::fs::File::create(dest)?;
    let (mut hasher, mut done, mut last) = (Sha256::new(), 0u64, 0.0f32);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;
        if let Some(t) = total {
            let f = done as f32 / t as f32;
            if f - last >= 0.01 {
                last = f;
                progress(Phase::Downloading, f);
            }
        }
    }
    out.flush()?;
    if let Some(expected) = &asset.sha256 {
        let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if !got.eq_ignore_ascii_case(expected) {
            bail!("empreinte SHA-256 incorrecte pour {} : téléchargement corrompu ou altéré", asset.name);
        }
    }
    Ok(())
}

fn extract(archive: &Path, name: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let lower = name.to_lowercase();
    if lower.ends_with(".zip") {
        zip::ZipArchive::new(std::fs::File::open(archive)?)?.extract(dest).context("extraction du zip")?;
    } else if lower.ends_with(".7z") {
        sevenz_rust::decompress_file(archive, dest).context("extraction du 7z")?;
    } else {
        bail!("format d'archive non géré : {name}");
    }
    Ok(())
}

/// Installe (ou met à jour) l'émulateur dans `<data>/emulators/<nom>/` et renvoie le chemin de son exécutable.
pub fn install(data: &Path, profile: &Profile, progress: &mut dyn FnMut(Phase, f32)) -> Result<PathBuf> {
    progress(Phase::Resolving, 0.0);
    let http = client()?;
    let asset = resolve(&http, &profile.source)?;
    tracing::info!("installation de {} : {}", profile.name, asset.url);

    let final_dir = managed_dir(data, profile);
    let work = data.join("emulators").join(format!(".tmp-{}", profile.name.to_lowercase()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;

    let result = (|| -> Result<()> {
        let archive = work.join(&asset.name);
        download(&http, &asset, &archive, progress)?;
        progress(Phase::Extracting, 0.0);
        let files = work.join("files");
        extract(&archive, &asset.name, &files)?;
        if find_in(&files, &profile.executable).is_none() {
            bail!("{} introuvable dans l'archive téléchargée", profile.executable);
        }
        // on ne touche à l'ancienne version qu'une fois la nouvelle vérifiée
        let _ = std::fs::remove_dir_all(&final_dir);
        std::fs::rename(&files, &final_dir).context("déplacement vers le dossier de l'émulateur")?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result?;
    let exe = find_in(&final_dir, &profile.executable).context("exécutable introuvable après installation")?;
    if profile.portable {
        // mode portable : BIOS, sauvegardes et réglages restent dans le dossier géré par EmuNest
        std::fs::write(exe.parent().unwrap_or(&final_dir).join("portable.txt"), b"")?;
    }
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assets(names: &[&str]) -> Vec<Asset> {
        names.iter().map(|n| Asset { name: n.to_string(), url: format!("https://x/{n}"), sha256: None }).collect()
    }

    #[test]
    fn picks_the_portable_windows_x64_archive() {
        let pick = |names: &[&str]| pick_asset(&assets(names)).map(|a| a.name.clone());
        assert_eq!(
            pick(&["pcsx2-v2.8.2-windows-x64-installer.exe", "pcsx2-v2.8.2-windows-x64-Qt-symbols.7z", "pcsx2-v2.8.2-windows-x64-Qt.7z", "pcsx2-v2.8.2-linux-appimage-x64-Qt.AppImage"]).as_deref(),
            Some("pcsx2-v2.8.2-windows-x64-Qt.7z")
        );
        assert_eq!(
            pick(&["duckstation-windows-arm64-release.zip", "duckstation-windows-x64-sse2-release.zip", "duckstation-windows-x64-release.zip", "duckstation-mac-release.zip"]).as_deref(),
            Some("duckstation-windows-x64-release.zip")
        );
        assert_eq!(pick(&["mGBA-0.10.5-win32.7z", "mGBA-0.10.5-win64.7z", "mGBA-0.10.5-win64-installer.exe"]).as_deref(), Some("mGBA-0.10.5-win64.7z"));
        assert_eq!(pick(&["PPSSPP-v1.20.4-Windows-ARM64.zip", "PPSSPP-v1.20.4-Windows-x64.zip"]).as_deref(), Some("PPSSPP-v1.20.4-Windows-x64.zip"));
        assert_eq!(
            pick(&["azahar-windows-mxe-2126.2.zip", "azahar-windows-msvc-2126.2.zip", "azahar-libretro-windows-x86_64-2126.2.zip", "azahar-windows-msys2-2126.2-installer.exe"]).as_deref(),
            Some("azahar-windows-msvc-2126.2.zip")
        );
        assert_eq!(pick(&["melonDS-1.1-windows-aarch64.zip", "melonDS-1.1-windows-x86_64.zip"]).as_deref(), Some("melonDS-1.1-windows-x86_64.zip"));
        assert_eq!(pick(&["source.tar.xz"]), None);
    }

    #[test]
    fn scrapes_first_matching_link() {
        let html = r#"<a href="https://dl.dolphin-emu.org/builds/09/39/dolphin-master-2609-308-x64.7z">x</a><a href="https://dl.dolphin-emu.org/builds/0a/05/dolphin-master-2609-293-x64.7z">"#;
        assert_eq!(first_link(html, "-x64.7z").as_deref(), Some("https://dl.dolphin-emu.org/builds/09/39/dolphin-master-2609-308-x64.7z"));
        assert_eq!(first_link(html, ".zip"), None);
    }

    /// Réseau : `cargo test -p emunest-core -- --ignored` installe vraiment mGBA (13 Mo) dans un dossier temporaire.
    #[test]
    #[ignore]
    fn live_install_mgba() {
        let data = std::env::temp_dir().join(format!("emunest-install-{}", std::process::id()));
        let profile = emunest_emulators::builtin_profiles().unwrap().into_iter().find(|p| p.name == "mGBA").unwrap();
        let mut last = 0.0;
        let exe = install(&data, &profile, &mut |_, f| last = f).unwrap();
        assert!(exe.is_file(), "{}", exe.display());
        assert_eq!(crate::launch::find_executable(&data, &profile), Some(exe));
        let _ = std::fs::remove_dir_all(&data);
    }

    /// Réseau : `EMUNEST_PROFILES=Cemu,RPCS3 cargo test -p emunest-core -- --ignored live_install_profiles`
    /// installe vraiment les émulateurs listés (grosses archives) pour vérifier les profils.
    #[test]
    #[ignore]
    fn live_install_profiles() {
        let wanted = std::env::var("EMUNEST_PROFILES").unwrap_or_default();
        let data = std::env::temp_dir().join(format!("emunest-install-all-{}", std::process::id()));
        for p in emunest_emulators::builtin_profiles().unwrap().into_iter().filter(|p| wanted.split(',').any(|w| w.eq_ignore_ascii_case(&p.name))) {
            let exe = install(&data, &p, &mut |_, _| {}).unwrap_or_else(|e| panic!("{} : {e:#}", p.name));
            assert!(exe.is_file(), "{}: {}", p.name, exe.display());
            eprintln!("OK {} -> {}", p.name, exe.display());
        }
        let _ = std::fs::remove_dir_all(&data);
    }
}
