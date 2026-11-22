//! Sons de l'interface, synthétisés au démarrage (aucun fichier audio) : tintements de verre doux,
//! arpèges courts, un peu de réverbération. Joués sur le périphérique par défaut dans un thread dédié.
pub mod bank;

use rodio::buffer::SamplesBuffer;
use std::collections::HashMap;
use std::num::NonZero;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const SR: u32 = 44_100;

/// Les sons de l'interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sound {
    /// déplacement dans une liste ou le carrousel
    Move,
    /// changement de console / filtre
    Filter,
    /// validation
    Select,
    /// retour / annulation
    Back,
    /// ouverture d'un menu ou d'une fenêtre
    Open,
    Close,
    /// lancement d'un jeu
    Launch,
    /// favori ajouté
    Favorite,
    /// touche du clavier à l'écran
    Key,
    /// message passager
    Toast,
    /// erreur
    Error,
    /// démarrage sans animation
    Start,
    /// animation de démarrage (≈ 4 s)
    Intro,
}

// ---------------------------------------------------------------------------------------------
// Synthèse : sons chauds et ronds (graves-médiums, attaques douces, filtre passe-bas, réverbération sombre)
// ---------------------------------------------------------------------------------------------

use std::f32::consts::PI;

const FS: f32 = SR as f32;

fn n(secs: f32) -> usize {
    (secs * FS) as usize
}

/// Note douce : partiels harmoniques, attaque en « smoothstep », décroissance exponentielle et petit glissando
/// de hauteur vers la note (`glide` = écart relatif de départ, ex. -0.08 = part 8 % plus bas) qui donne le côté « bloop ».
fn synth_tone(freq: f32, secs: f32, attack: f32, decay: f32, partials: &[(f32, f32)], glide: f32, amp: f32) -> Vec<f32> {
    let mut phase = 0.0f32;
    let total = n(secs);
    (0..total)
        .map(|i| {
            let t = i as f32 / FS;
            let f = freq * (1.0 + glide * (-t / 0.045).exp());
            phase += 2.0 * PI * f / FS;
            let a = (t / attack).min(1.0);
            let env = a * a * (3.0 - 2.0 * a) * (-t / decay).exp();
            let s: f32 = partials.iter().map(|(m, g)| g * (phase * m).sin()).sum();
            // fondu de sortie sur les 25 dernières ms : jamais de clic
            let tail = ((total - i) as f32 / (0.025 * FS)).min(1.0);
            s * env * tail * amp
        })
        .collect()
}

/// Un son rond : fondamentale + un peu de 2e et 3e harmonique.
const ROUND: &[(f32, f32)] = &[(1.0, 1.0), (2.0, 0.16), (3.0, 0.04)];
/// Plus pur, pour les notes aiguës.
const PURE: &[(f32, f32)] = &[(1.0, 1.0), (2.0, 0.07)];

/// Souffle d'air : bruit filtré dont la brillance monte (`rising`) ou descend, avec une enveloppe en cloche.
fn swish(secs: f32, rising: bool, amp: f32) -> Vec<f32> {
    let total = n(secs);
    let (mut seed, mut y) = (0x9E37_79B9u32, 0.0f32);
    (0..total)
        .map(|i| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0;
            let p = i as f32 / total as f32;
            let bell = (PI * p).sin().powi(2);
            let fc = if rising { 300.0 + 1500.0 * p } else { 1800.0 - 1500.0 * p };
            y += (1.0 - (-2.0 * PI * fc / FS).exp()) * (noise - y);
            y * bell * amp
        })
        .collect()
}

/// Filtre passe-bas à un pôle : adoucit tout ce qui dépasse `cutoff` Hz.
fn lowpass(buf: &mut [f32], cutoff: f32) {
    let a = 1.0 - (-2.0 * PI * cutoff / FS).exp();
    let mut y = 0.0;
    for s in buf.iter_mut() {
        y += a * (*s - y);
        *s = y;
    }
}

/// Place des sons les uns après les autres (décalages en millisecondes).
fn mix(parts: Vec<(u32, Vec<f32>)>) -> Vec<f32> {
    let len = parts.iter().map(|(ms, b)| (ms * SR / 1000) as usize + b.len()).max().unwrap_or(0);
    let mut out = vec![0.0; len];
    for (ms, buf) in parts {
        let off = (ms * SR / 1000) as usize;
        for (i, s) in buf.iter().enumerate() {
            out[off + i] += s;
        }
    }
    out
}

/// Réverbération sombre (4 peignes amortis + 2 passe-tout, à la Schroeder) : de l'espace, sans brillance.
fn reverb(dry: Vec<f32>, wet: f32, tail: f32) -> Vec<f32> {
    let len = dry.len() + n(tail);
    let mut input = dry.clone();
    input.resize(len, 0.0);
    let mut acc = vec![0.0f32; len];
    for (delay_ms, fb) in [(29.7f32, 0.80f32), (37.1, 0.78), (41.1, 0.76), (43.7, 0.74)] {
        let d = n(delay_ms / 1000.0);
        let mut line = vec![0.0f32; len];
        let mut damp = 0.0f32;
        for i in 0..len {
            let echo = if i >= d { line[i - d] } else { 0.0 };
            damp += 0.45 * (echo - damp); // amortit les aigus dans la boucle de retour
            line[i] = input[i] + damp * fb;
            acc[i] += line[i] * 0.25;
        }
    }
    for (delay_ms, g) in [(5.0f32, 0.7f32), (1.7, 0.7)] {
        let d = n(delay_ms / 1000.0);
        let src = acc.clone();
        for i in 0..len {
            let delayed = if i >= d { src[i - d] } else { 0.0 };
            let out_prev = if i >= d { acc[i - d] } else { 0.0 };
            acc[i] = -g * src[i] + delayed + g * out_prev;
        }
    }
    lowpass(&mut acc, 2600.0);
    dry.iter().chain(std::iter::repeat(&0.0)).zip(&acc).map(|(d, w)| d + w * wet).collect()
}

/// Normalise le pic à `peak`, adoucit encore les aigus et ajoute un fondu final (pas de clic).
fn finish(mut buf: Vec<f32>, peak: f32) -> Vec<f32> {
    lowpass(&mut buf, 4200.0);
    let max = buf.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6);
    let fade = n(0.03);
    let len = buf.len();
    for (i, s) in buf.iter_mut().enumerate() {
        *s *= peak / max;
        if i + fade > len {
            *s *= ((len - i) as f32 / fade as f32).powi(2);
        }
    }
    buf
}

/// Nappe douce : plusieurs sinus légèrement désaccordés (effet chœur), attaque lente, longue décroissance.
fn pad(freq: f32, secs: f32, attack: f32, release: f32, amp: f32) -> Vec<f32> {
    let total = n(secs);
    (0..total)
        .map(|i| {
            let t = i as f32 / FS;
            let a = (t / attack).min(1.0);
            let env = a * a * (3.0 - 2.0 * a) * (-((t - attack).max(0.0)) / release).exp();
            let mut s = 0.0;
            for (detune, g) in [(-0.0016f32, 0.6f32), (0.0, 1.0), (0.0018, 0.6)] {
                let f = freq * (1.0 + detune);
                s += g * ((2.0 * PI * f * t).sin() + 0.18 * (2.0 * PI * 2.0 * f * t).sin());
            }
            let tail = ((total - i) as f32 / (0.08 * FS)).min(1.0);
            s * env * tail * amp
        })
        .collect()
}

/// Échantillons de l'utilisateur (facultatifs) : `bell` pour les notes courtes, `chime` pour les notes longues et résonantes.
/// Sans eux, tout est synthétisé.
#[derive(Default, Clone, Debug)]
pub struct Bank {
    pub bell: Option<bank::Sample>,
    pub chime: Option<bank::Sample>,
    /// nappe (pad.*) pour l'intro et le lancement
    pub pad: Option<bank::Sample>,
    /// basse profonde (`bass.*`, un 808 par exemple) pour l'intro
    pub bass: Option<bank::Sample>,
}

/// Charge `bell.*` et `chime.*` (wav, ogg, flac, mp3) du dossier ; un fichier illisible est simplement ignoré.
pub fn load_bank(dir: &std::path::Path) -> Bank {
    let find_with = |stem: &str, load: fn(&std::path::Path) -> anyhow::Result<bank::Sample>| -> Option<bank::Sample> {
        std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| {
            p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case(stem))
        }).and_then(|p| load(&p).map_err(|e| tracing::warn!("échantillon ignoré : {e:#}")).ok())
    };
    let find = |stem: &str| find_with(stem, bank::load);
    Bank { bell: find("bell"), chime: find("chime"), pad: find("pad"), bass: find_with("bass", bank::load_bass) }
}

/// Une note : l'échantillon de l'utilisateur à la bonne hauteur s'il y en a un, sinon la synthèse.
/// Les notes longues (≥ 0,45 s) préfèrent le `chime`, les courtes le `bell`.
#[allow(clippy::too_many_arguments)]
fn voice(bank: &Bank, freq: f32, secs: f32, attack: f32, decay: f32, partials: &[(f32, f32)], glide: f32, amp: f32) -> Vec<f32> {
    let sample = if secs >= 0.45 { bank.chime.as_ref().or(bank.bell.as_ref()) } else { bank.bell.as_ref() };
    match sample {
        Some(s) => s.note(freq, secs, attack.max(0.006), decay.max(0.04), amp),
        None => synth_tone(freq, secs, attack, decay, partials, glide, amp),
    }
}

#[cfg(test)]
fn render(sound: Sound) -> Vec<f32> {
    render_with(sound, &Bank::default())
}

fn render_with(sound: Sound, bank: &Bank) -> Vec<f32> {
    use Sound::*;
    // même nom et mêmes paramètres que la synthèse : chaque note passe par l'échantillon s'il existe
    let tone = |freq: f32, secs: f32, attack: f32, decay: f32, partials: &[(f32, f32)], glide: f32, amp: f32| {
        voice(bank, freq, secs, attack, decay, partials, glide, amp)
    };
    // nappes : l'échantillon réel s'il existe ; le souffle bruité (le plus « synthé ») est presque coupé dans ce cas
    let pad = |freq: f32, secs: f32, attack: f32, release: f32, amp: f32| match &bank.pad {
        Some(p) => p.note(freq, secs, attack, release * 2.0, amp * 0.8),
        None => pad(freq, secs, attack, release, amp),
    };
    let swish = |secs: f32, rising: bool, amp: f32| swish(secs, rising, if bank.bell.is_some() { amp * 0.15 } else { amp });
    match sound {
        // déplacement : une goutte douce, à peine perceptible
        Move => finish(tone(392.0, 0.16, 0.010, 0.050, ROUND, -0.06, 1.0), 0.17),
        // changement de console : un souffle et deux notes qui se suivent
        Filter => finish(
            reverb(
                mix(vec![
                    (0, swish(0.22, true, 0.5)),
                    (20, tone(330.0, 0.24, 0.012, 0.07, ROUND, -0.05, 0.8)),
                    (95, tone(440.0, 0.34, 0.014, 0.10, ROUND, -0.05, 1.0)),
                ]),
                0.35,
                0.25,
            ),
            0.24,
        ),
        // validation : sol → do, montant et chaleureux
        Select => finish(
            reverb(mix(vec![(0, tone(392.0, 0.26, 0.012, 0.09, ROUND, -0.05, 0.85)), (85, tone(523.25, 0.46, 0.014, 0.16, ROUND, -0.05, 1.0))]), 0.4, 0.3),
            0.28,
        ),
        // retour : do → sol, descendant et doux
        Back => finish(
            reverb(mix(vec![(0, tone(523.25, 0.22, 0.012, 0.07, ROUND, 0.04, 0.8)), (75, tone(349.23, 0.40, 0.014, 0.13, ROUND, 0.04, 0.95))]), 0.4, 0.28),
            0.23,
        ),
        // ouverture : un souffle qui monte puis deux notes
        Open => finish(
            reverb(
                mix(vec![
                    (0, swish(0.30, true, 0.55)),
                    (60, tone(293.66, 0.30, 0.016, 0.09, ROUND, -0.05, 0.8)),
                    (150, tone(440.0, 0.50, 0.018, 0.16, ROUND, -0.05, 1.0)),
                ]),
                0.45,
                0.32,
            ),
            0.27,
        ),
        // fermeture : le souffle redescend
        Close => finish(
            reverb(
                mix(vec![
                    (0, swish(0.26, false, 0.5)),
                    (10, tone(440.0, 0.24, 0.012, 0.07, ROUND, 0.04, 0.8)),
                    (90, tone(293.66, 0.40, 0.014, 0.12, ROUND, 0.04, 0.95)),
                ]),
                0.4,
                0.3,
            ),
            0.22,
        ),
        // lancement : une nappe qui gonfle puis un arpège qui s'ouvre
        Launch => finish(
            reverb(
                mix(vec![
                    (0, swish(0.55, true, 0.6)),
                    (0, pad(130.81, 1.4, 0.35, 0.5, 0.8)),
                    (0, pad(196.0, 1.4, 0.40, 0.5, 0.55)),
                    (160, tone(523.25, 0.55, 0.02, 0.20, PURE, -0.03, 0.8)),
                    (260, tone(659.25, 0.55, 0.02, 0.20, PURE, -0.03, 0.85)),
                    (360, tone(783.99, 0.65, 0.02, 0.24, PURE, -0.03, 0.9)),
                    (470, tone(1046.5, 0.90, 0.02, 0.32, PURE, -0.03, 0.8)),
                ]),
                0.55,
                0.5,
            ),
            0.36,
        ),
        // favori : trois gouttes qui montent
        Favorite => finish(
            reverb(
                mix(vec![
                    (0, tone(523.25, 0.24, 0.016, 0.07, PURE, -0.03, 0.8)),
                    (70, tone(659.25, 0.24, 0.016, 0.07, PURE, -0.03, 0.85)),
                    (140, tone(783.99, 0.45, 0.018, 0.14, PURE, -0.03, 0.9)),
                ]),
                0.45,
                0.3,
            ),
            0.22,
        ),
        // touche du clavier à l'écran : une pression feutrée
        Key => finish(tone(311.0, 0.09, 0.006, 0.030, ROUND, -0.05, 1.0), 0.15),
        Toast => finish(reverb(tone(440.0, 0.34, 0.014, 0.10, ROUND, -0.05, 1.0), 0.4, 0.25), 0.19),
        // erreur : deux notes graves qui retombent, sans agressivité
        Error => finish(
            reverb(mix(vec![(0, tone(220.0, 0.26, 0.014, 0.09, ROUND, 0.0, 0.9)), (110, tone(174.61, 0.46, 0.016, 0.16, ROUND, 0.0, 1.0))]), 0.3, 0.25),
            0.24,
        ),
        // démarrage sans animation : un accord clair et court
        Start => finish(
            reverb(
                mix(vec![
                    (0, tone(261.63, 0.7, 0.02, 0.25, ROUND, -0.03, 0.8)),
                    (90, tone(329.63, 0.7, 0.02, 0.25, ROUND, -0.03, 0.85)),
                    (180, tone(392.0, 0.9, 0.02, 0.30, ROUND, -0.03, 0.9)),
                ]),
                0.5,
                0.5,
            ),
            0.28,
        ),
        // intro, tout en Do : basse, nappe, puis deux notes (Do, Sol)
        Intro => finish(
            reverb(
                mix(vec![
                    (0, match &bank.bass { Some(b) => b.note(65.41, 4.2, 0.02, 1.6, 1.0), None => pad(65.41, 4.2, 1.3, 1.4, 0.9) }),
                    (0, pad(130.81, 4.2, 1.2, 1.5, 0.8)),
                    (1300, tone(523.25, 1.8, 0.05, 0.60, PURE, 0.0, 0.5)),
                    (1800, tone(783.99, 2.0, 0.05, 0.70, PURE, 0.0, 0.5)),
                ]),
                0.65,
                1.0,
            ),
            0.40,
        ),
    }
}

/// Tous les sons rendus en mémoire (pour les tests et l'aperçu WAV).
pub fn render_all() -> Vec<(Sound, Vec<f32>)> {
    render_all_with(&Bank::default())
}

/// Idem, avec les échantillons de l'utilisateur.
pub fn render_all_with(bank: &Bank) -> Vec<(Sound, Vec<f32>)> {
    use Sound::*;
    [Move, Filter, Select, Back, Open, Close, Launch, Favorite, Key, Toast, Error, Start, Intro].into_iter().map(|s| (s, render_with(s, bank))).collect()
}

/// Écrit un WAV 16 bits mono (aperçu hors application).
pub fn write_wav(path: &std::path::Path, samples: &[f32]) -> std::io::Result<()> {
    let data: Vec<u8> = samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
    let mut out = Vec::with_capacity(44 + data.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SR.to_le_bytes());
    out.extend_from_slice(&(SR * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    std::fs::write(path, out)
}

// ---------------------------------------------------------------------------------------------
// Lecture
// ---------------------------------------------------------------------------------------------

struct Player {
    tx: Mutex<Sender<Sound>>,
    enabled: AtomicBool,
    /// volume en millièmes (0..=1000)
    volume: AtomicU32,
}

static PLAYER: OnceLock<Player> = OnceLock::new();

/// Démarre le thread audio. Renvoie vrai si un périphérique audio a été ouvert ; sinon tout reste silencieux (aucune erreur).
pub fn init(enabled: bool, volume: f32, sounds_dir: Option<&std::path::Path>) -> bool {
    let (tx, rx) = channel::<Sound>();
    let player = Player { tx: Mutex::new(tx), enabled: AtomicBool::new(enabled), volume: AtomicU32::new((volume.clamp(0.0, 1.0) * 1000.0) as u32) };
    if PLAYER.set(player).is_err() {
        return true;
    }
    let (ready_tx, ready_rx) = channel::<bool>();
    let sounds_dir = sounds_dir.map(|d| d.to_path_buf());
    std::thread::spawn(move || {
        let sink = match rodio::DeviceSinkBuilder::open_default_sink() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("audio indisponible : {e}");
                let _ = ready_tx.send(false);
                return;
            }
        };
        let _ = ready_tx.send(true);
        let bank = sounds_dir.as_deref().map(load_bank).unwrap_or_default();
        if bank.bell.is_some() || bank.chime.is_some() {
            tracing::info!("sons : échantillons de l'utilisateur chargés");
        }
        let buffers: HashMap<Sound, Vec<f32>> = render_all_with(&bank).into_iter().collect();
        let mut last: HashMap<Sound, Instant> = HashMap::new();
        while let Ok(sound) = rx.recv() {
            let Some(p) = PLAYER.get() else { continue };
            if !p.enabled.load(Ordering::Relaxed) {
                continue;
            }
            // anti-rafale : le même son pas plus d'une fois toutes les 35 ms (flèche maintenue)
            let now = Instant::now();
            if last.get(&sound).is_some_and(|t| now.duration_since(*t) < Duration::from_millis(35)) {
                continue;
            }
            last.insert(sound, now);
            let vol = p.volume.load(Ordering::Relaxed) as f32 / 1000.0;
            let data: Vec<f32> = buffers[&sound].iter().map(|s| s * vol).collect();
            sink.mixer().add(SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(SR).unwrap(), data));
        }
    });
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
}

/// Joue un son (sans bloquer). Sans effet si `init` n'a pas été appelé ou si les sons sont coupés.
pub fn play(sound: Sound) {
    if let Some(p) = PLAYER.get() {
        if p.enabled.load(Ordering::Relaxed) {
            let _ = p.tx.lock().map(|tx| tx.send(sound));
        }
    }
}

pub fn set_enabled(enabled: bool) {
    if let Some(p) = PLAYER.get() {
        p.enabled.store(enabled, Ordering::Relaxed);
    }
}

pub fn set_volume(volume: f32) {
    if let Some(p) = PLAYER.get() {
        p.volume.store((volume.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed);
    }
}

pub fn is_enabled() -> bool {
    PLAYER.get().is_some_and(|p| p.enabled.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proportion d'énergie dans les variations rapides (différence première) : ~0,013 pour un sinus à 800 Hz.
    /// Un son « brillant » (cloche, aigus) dépasse largement 0,05 ; les nôtres doivent rester chauds.
    fn brightness(buf: &[f32]) -> f32 {
        let e: f32 = buf.iter().map(|s| s * s).sum();
        let d: f32 = buf.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum();
        d / e.max(1e-9)
    }

    #[test]
    fn every_sound_is_finite_quiet_enough_and_click_free() {
        for (sound, buf) in render_all() {
            assert!(!buf.is_empty(), "{sound:?} vide");
            assert!(buf.iter().all(|s| s.is_finite()), "{sound:?} contient NaN/inf");
            let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!((0.05..=0.45).contains(&peak), "{sound:?} : pic {peak} hors de la plage douce");
            assert!(buf.last().unwrap().abs() < 0.01, "{sound:?} ne finit pas à zéro (clic)");
            assert!(buf[0].abs() < 0.01, "{sound:?} ne commence pas à zéro (clic)");
            let secs = buf.len() as f32 / SR as f32;
            assert!(secs < 5.5, "{sound:?} trop long : {secs} s");
        }
    }

    #[test]
    fn sounds_are_warm_not_bright() {
        for (sound, buf) in render_all() {
            let b = brightness(&buf);
            assert!(b < 0.045, "{sound:?} trop brillant : {b:.4}");
        }
    }

    #[test]
    fn short_ui_sounds_stay_short_and_the_intro_is_long_enough() {
        let get = |s: Sound| render(s).len() as f32 / SR as f32;
        assert!(get(Sound::Move) < 0.25);
        assert!(get(Sound::Key) < 0.2);
        assert!(get(Sound::Intro) > 3.0);
    }

    #[test]
    fn sounds_built_from_user_samples_stay_smooth_and_safe() {
        // un faux « bell » (sinus qui s'éteint + un partiel) joue le rôle des échantillons de l'utilisateur
        let dir = std::env::temp_dir().join(format!("emunest-bank-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = |name: &str, freq: f32, secs: f32| {
            let rate = 44_100u32;
            let samples: Vec<i16> = (0..(rate as f32 * secs) as usize)
                .map(|i| {
                    let t = i as f32 / rate as f32;
                    (((2.0 * PI * freq * t).sin() * 0.8 + (2.0 * PI * freq * 2.76 * t).sin() * 0.15) * (-t / 0.5).exp() * 30000.0) as i16
                })
                .collect();
            let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            let mut out = Vec::new();
            out.extend_from_slice(b"RIFF");
            out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
            out.extend_from_slice(b"WAVEfmt ");
            for v in [16u32.to_le_bytes().to_vec(), 1u16.to_le_bytes().to_vec(), 1u16.to_le_bytes().to_vec(), rate.to_le_bytes().to_vec(), (rate * 2).to_le_bytes().to_vec(), 2u16.to_le_bytes().to_vec(), 16u16.to_le_bytes().to_vec()] {
                out.extend_from_slice(&v);
            }
            out.extend_from_slice(b"data");
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&data);
            std::fs::write(dir.join(name), out).unwrap();
        };
        wav("bell.wav", 523.25, 0.6);
        wav("chime.wav", 261.63, 2.5);
        let bank = load_bank(&dir);
        assert!(bank.bell.is_some() && bank.chime.is_some());
        for (sound, buf) in render_all_with(&bank) {
            assert!(buf.iter().all(|s| s.is_finite()), "{sound:?}");
            let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!((0.05..=0.45).contains(&peak), "{sound:?} pic {peak}");
            assert!(buf[0].abs() < 0.01 && buf.last().unwrap().abs() < 0.01, "{sound:?} clic");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wav_has_a_valid_header() {
        let dir = std::env::temp_dir().join(format!("emunest-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        write_wav(&path, &render(Sound::Select)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(bytes.len(), 44 + render(Sound::Select).len() * 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
