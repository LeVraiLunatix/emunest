//! Jaquettes et captures de jeu automatiques via libretro-thumbnails (gratuit, sans clé), cache disque.
//! Les noms de fichiers du dépôt sont listés une fois par plateforme (API GitHub), puis le
//! titre du jeu est rapproché du meilleur nom (mots communs + préférence de région).
use anyhow::{bail, Context, Result};
use image::DynamicImage;
use emunest_core::Game;
use emunest_emulators::libretro_system;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const CARD_W: u32 = 300;
const CARD_RADIUS: f32 = 34.0;
const LIST_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn key(g: &Game) -> String {
    format!("{}-{:016x}", g.platform, fnv(&g.title))
}

fn file(dir: &Path, g: &Game, ext: &str) -> PathBuf {
    dir.join("covers").join(format!("{}.{ext}", key(g)))
}

/// Jaquette en cache pour ce jeu, si elle existe.
pub fn cover_path(dir: &Path, g: &Game) -> Option<PathBuf> {
    let p = file(dir, g, "card.png");
    p.exists().then_some(p)
}

/// Trois couleurs d'ambiance tirées de la jaquette : le pixel le plus vif de chaque tiers (gauche, centre, droite),
/// ramené à une saturation et une luminosité qui rendent bien en lueur.
pub fn palette(dir: &Path, g: &Game) -> Option<[(u8, u8, u8); 3]> {
    let img = image::open(bg_path(dir, g)?).ok()?.to_rgb8();
    let (w, h) = img.dimensions();
    let third = |i: u32| {
        let best = (w * i / 3..w * (i + 1) / 3)
            .flat_map(|x| (0..h).map(move |y| (x, y)))
            .map(|(x, y)| img.get_pixel(x, y).0)
            .max_by(|a, b| vividness(*a).partial_cmp(&vividness(*b)).unwrap())
            .unwrap_or([60, 60, 90]);
        glow(best)
    };
    Some([third(1), third(0), third(2)])
}

fn vividness(p: [u8; 3]) -> f32 {
    let (mx, mn) = (p.iter().max().copied().unwrap() as f32, p.iter().min().copied().unwrap() as f32);
    (mx - mn) * (0.4 + mx / 255.0)
}

/// RVB -> TSL, saturation poussée et luminosité bornée -> RVB.
fn glow(p: [u8; 3]) -> (u8, u8, u8) {
    let [r, g, b] = p.map(|c| c as f32 / 255.0);
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let l = (mx + mn) / 2.0;
    let d = mx - mn;
    let s = if d == 0.0 { 0.0 } else { d / (1.0 - (2.0 * l - 1.0).abs()) };
    let hue = if d == 0.0 {
        0.0
    } else if mx == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if mx == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let (s, l) = ((s * 2.5).clamp(0.2, 0.9), l.clamp(0.42, 0.58));
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((hue.rem_euclid(360.0) / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (hue.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let u = |v: f32| ((v + m) * 255.0).round() as u8;
    (u(r), u(g), u(b))
}

/// Miniature très réduite et floutée de la jaquette (agrandie : flou doux de repli quand il n'y a pas de capture).
pub fn bg_path(dir: &Path, g: &Game) -> Option<PathBuf> {
    let cover = cover_path(dir, g)?;
    let bg = file(dir, g, "bg2.png");
    if !bg.exists() {
        let small = image::open(&cover).ok()?.resize_exact(48, 68, image::imageops::FilterType::Triangle).to_rgb8();
        image::imageops::blur(&small, 3.0).save_with_format(&bg, image::ImageFormat::Png).ok()?;
    }
    Some(bg)
}

/// Mots significatifs d'un nom : sans (régions) ni [tags], minuscules, sans ponctuation ni "the".
fn tokens(name: &str) -> HashSet<String> {
    let mut depth = 0;
    let flat: String = name
        .chars()
        .filter(|&c| match c {
            '(' | '[' => {
                depth += 1;
                false
            }
            ')' | ']' => {
                depth = (depth - 1).max(0);
                false
            }
            _ => depth == 0,
        })
        .collect();
    flat.to_lowercase()
        .replace(['\'', '’'], "")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && *w != "the")
        .map(str::to_string)
        .collect()
}

fn region_rank(name: &str, wanted: &str) -> i32 {
    let base = if name.contains("Europe") || name.contains("France") {
        3
    } else if name.contains("World") {
        2
    } else if name.contains("USA") {
        1
    } else {
        0
    };
    // même région que le fichier local : prioritaire
    let same = ["Europe", "France", "USA", "Japan", "World"]
        .iter()
        .any(|r| name.contains(r) && wanted.contains(r));
    base + if same { 10 } else { 0 }
}

/// Meilleur nom du dépôt pour `stem` (nom de fichier local sans extension).
fn best_match<'a>(stem: &str, names: &'a [String]) -> Option<&'a str> {
    let want = tokens(stem);
    if want.is_empty() {
        return None;
    }
    names
        .iter()
        .filter_map(|n| {
            let have = tokens(n);
            let inter = want.intersection(&have).count() as f32;
            let score = 2.0 * inter / (want.len() + have.len()) as f32; // coefficient de Dice
            (score >= 0.7).then(|| (n, score, region_rank(n, stem)))
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap().then(a.2.cmp(&b.2)).then(b.0.len().cmp(&a.0.len())))
        .map(|(n, _, _)| n.as_str())
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn repo_of(system: &str) -> String {
    system.replace(' ', "_").replace('&', "_")
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder().user_agent("emunest").timeout(Duration::from_secs(30)).build()?)
}

/// Noms des jaquettes d'une plateforme (cache disque 30 jours).
fn list_names(dir: &Path, http: &reqwest::blocking::Client, system: &str) -> Result<Vec<String>> {
    let file = dir.join("lists").join(format!("{}.txt", repo_of(system)));
    let fresh = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .map(|t| SystemTime::now().duration_since(t).unwrap_or_default() < LIST_MAX_AGE)
        .unwrap_or(false);
    if fresh {
        return Ok(std::fs::read_to_string(&file)?.lines().map(str::to_string).collect());
    }
    let url = format!(
        "https://api.github.com/repos/libretro-thumbnails/{}/git/trees/master:Named_Boxarts",
        repo_of(system)
    );
    let resp = http.get(url).send()?;
    if !resp.status().is_success() {
        bail!("liste des jaquettes {system} : HTTP {}", resp.status());
    }
    let json: serde_json::Value = serde_json::from_slice(&resp.bytes()?)?;
    let names: Vec<String> = json["tree"]
        .as_array()
        .context("réponse GitHub inattendue")?
        .iter()
        .filter_map(|e| e["path"].as_str())
        .filter_map(|p| p.strip_suffix(".png"))
        .map(str::to_string)
        .collect();
    std::fs::create_dir_all(file.parent().unwrap())?;
    std::fs::write(&file, names.join("\n"))?;
    Ok(names)
}

/// Met l'image à la largeur des jaquettes en gardant ses vraies proportions (bornées : l'interface ne
/// sait pas afficher du très étroit ou du très large) et arrondit les coins (alpha) : Slint ne sait pas arrondir une image.
fn to_card(img: DynamicImage) -> image::RgbaImage {
    let aspect = (img.width() as f32 / img.height() as f32).clamp(0.55, 1.1);
    let card_h = (CARD_W as f32 / aspect).round() as u32;
    let mut card = img.resize_to_fill(CARD_W, card_h, image::imageops::FilterType::Triangle).to_rgba8();
    let r = CARD_RADIUS;
    for (x, y, px) in card.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let cx = if fx < r { r } else if fx > CARD_W as f32 - r { CARD_W as f32 - r } else { continue };
        let cy = if fy < r { r } else if fy > card_h as f32 - r { card_h as f32 - r } else { continue };
        let cover = (r - (fx - cx).hypot(fy - cy) + 0.5).clamp(0.0, 1.0);
        px.0[3] = (px.0[3] as f32 * cover) as u8;
    }
    card
}

fn save_card(http: &reqwest::blocking::Client, url: &str, dest: &Path) -> Result<()> {
    let bytes = http.get(url).send()?.error_for_status()?.bytes()?;
    let tmp = dest.with_extension("part");
    to_card(image::load_from_memory(&bytes)?).save_with_format(&tmp, image::ImageFormat::Png)?;
    std::fs::rename(tmp, dest)?;
    Ok(())
}

fn download(http: &reqwest::blocking::Client, system: &str, name: &str, dest: &Path) -> Result<()> {
    save_card(
        http,
        &format!(
            "https://raw.githubusercontent.com/libretro-thumbnails/{}/master/Named_Boxarts/{}.png",
            repo_of(system),
            encode(name)
        ),
        dest,
    )
}

/// Repli sans clé : l'image principale (le boîtier) de la page Wikipédia du jeu. Sert aux consoles
/// absentes de libretro-thumbnails (Switch…) et aux titres que libretro ne connaît pas.
/// On n'accepte la page que si son titre ressemble vraiment à celui du jeu.
fn wikipedia_cover(http: &reqwest::blocking::Client, title: &str) -> Result<Option<String>> {
    let url = reqwest::Url::parse_with_params(
        "https://en.wikipedia.org/w/api.php",
        &[
            ("action", "query"),
            ("generator", "search"),
            ("gsrsearch", &format!("{title} video game")),
            ("gsrlimit", "5"),
            ("prop", "pageimages"),
            ("piprop", "original"),
            ("pilicense", "any"), // les boîtiers sont des images non libres
            ("format", "json"),
        ],
    )?;
    let json: serde_json::Value = serde_json::from_slice(&http.get(url).send()?.error_for_status()?.bytes()?)?;
    let want = tokens(title);
    let best = json["query"]["pages"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, p)| {
            let src = p["original"]["source"].as_str()?;
            let have = tokens(p["title"].as_str()?);
            let score = 2.0 * want.intersection(&have).count() as f32 / (want.len() + have.len()).max(1) as f32;
            (score >= 0.7).then(|| (src.to_string(), score, p["index"].as_i64().unwrap_or(99)))
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap().then(b.2.cmp(&a.2)));
    Ok(best.map(|(src, _, _)| src))
}

/// Jaquette choisie par l'utilisateur : remplace celle en cache (et sera conservée : plus de recherche automatique).
pub fn set_custom_cover(dir: &Path, g: &Game, src: &Path) -> Result<()> {
    let img = image::open(src).with_context(|| format!("image illisible : {}", src.display()))?;
    std::fs::create_dir_all(dir.join("covers"))?;
    let dest = file(dir, g, "card.png");
    let tmp = dest.with_extension("part");
    to_card(img).save_with_format(&tmp, image::ImageFormat::Png)?;
    std::fs::rename(tmp, &dest)?;
    let _ = std::fs::remove_file(file(dir, g, "bg2.png")); // le fond flou et les couleurs sont refaits
    let _ = std::fs::remove_file(file(dir, g, "none"));
    Ok(())
}

/// Oublie la jaquette d'un jeu (et le « introuvable » mémorisé) : la prochaine recherche repart de zéro.
pub fn reset_cover(dir: &Path, g: &Game) {
    for ext in ["card.png", "bg2.png", "none"] {
        let _ = std::fs::remove_file(file(dir, g, ext));
    }
}

/// Télécharge les jaquettes manquantes (une à la fois) ; `on_cover` est appelé à chaque nouvelle jaquette.
/// Ordre : libretro-thumbnails, puis Wikipédia. Un jeu sans correspondance est mémorisé (.none)
/// pour ne pas être redemandé à chaque lancement.
pub fn fetch_missing(dir: &Path, games: &[Game], on_cover: &mut dyn FnMut(&Game)) {
    let _ = std::fs::create_dir_all(dir.join("covers"));
    let http = match client() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("jaquettes : {e:#}");
            return;
        }
    };
    let mut lists: HashMap<&str, Option<Vec<String>>> = HashMap::new();

    for g in games {
        let (dest, none) = (file(dir, g, "card.png"), file(dir, g, "none"));
        if dest.exists() || none.exists() {
            continue;
        }
        // 1. libretro-thumbnails (Err = réseau : on réessaiera, rien n'est mémorisé)
        let mut reliable = true; // vrai tant qu'un « introuvable » est une vraie réponse
        if let Some(system) = libretro_system(&g.platform) {
            let names = lists
                .entry(system)
                .or_insert_with(|| list_names(dir, &http, system).map_err(|e| tracing::warn!("{e:#}")).ok());
            match names {
                None => reliable = false,
                Some(names) => {
                    let stem = g.path.file_stem().and_then(|s| s.to_str()).unwrap_or(&g.title);
                    if let Some(name) = best_match(stem, names) {
                        match download(&http, system, name, &dest) {
                            Ok(()) => on_cover(g),
                            Err(e) => tracing::warn!("jaquette « {} » : {e:#}", g.title),
                        }
                        continue;
                    }
                }
            }
        }
        // 2. Wikipédia
        match wikipedia_cover(&http, &g.title) {
            Ok(Some(url)) => match save_card(&http, &url, &dest) {
                Ok(()) => on_cover(g),
                Err(e) => tracing::warn!("jaquette Wikipédia « {} » : {e:#}", g.title),
            },
            Ok(None) if reliable => {
                let _ = std::fs::write(&none, b"");
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("Wikipédia « {} » : {e:#}", g.title),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_title_and_prefers_region() {
        let names: Vec<String> = [
            "God of War (USA)",
            "God of War (Europe, Australia) (En,Fr,De,Es,It)",
            "God of War II (USA)",
            "Gran Turismo 4 (Europe)",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let eu = "God of War (Europe, Australia) (En,Fr,De,Es,It)";
        assert_eq!(best_match("God of War (Europe)", &names), Some(eu));
        assert_eq!(best_match("God of War", &names), Some(eu));
        assert_eq!(best_match("Totally Unknown Game", &names), None);
    }

    #[test]
    fn encodes_like_url() {
        assert_eq!(encode("A (B).png"), "A%20%28B%29.png");
    }
}
