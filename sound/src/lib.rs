//! Sons de l'interface, synthétisés au démarrage (aucun fichier audio) : tintements de verre doux,
//! arpèges courts, un peu de réverbération. Joués sur le périphérique par défaut dans un thread dédié.
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
    /// démarrage d'EmuNest
    Start,
}

// ---------------------------------------------------------------------------------------------
// Synthèse
// ---------------------------------------------------------------------------------------------

/// Une note : somme de partiels (multiple de fréquence, amplitude) avec attaque de 2 ms et décroissance exponentielle.
fn note(freq: f32, secs: f32, decay: f32, partials: &[(f32, f32)], amp: f32) -> Vec<f32> {
    let n = (secs * SR as f32) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR as f32;
            let env = (1.0 - (-t / 0.002).exp()) * (-t / decay).exp();
            let s: f32 = partials.iter().map(|(m, a)| a * (2.0 * std::f32::consts::PI * freq * m * t).sin()).sum();
            s * env * amp
        })
        .collect()
}

/// Partiels d'un tintement de verre (légèrement inharmoniques, comme une cloche fine).
const GLASS: &[(f32, f32)] = &[(1.0, 1.0), (2.76, 0.32), (5.40, 0.12)];
/// Partiels d'un son plus rond (touche, erreur).
const SOFT: &[(f32, f32)] = &[(1.0, 1.0), (2.0, 0.22), (3.0, 0.08)];

/// Place des sons les uns après les autres (décalages en millisecondes).
fn mix(parts: Vec<(u32, Vec<f32>)>) -> Vec<f32> {
    let len = parts.iter().map(|(ms, b)| ms * SR / 1000 + b.len() as u32).max().unwrap_or(0) as usize;
    let mut out = vec![0.0; len];
    for (ms, buf) in parts {
        let off = (ms * SR / 1000) as usize;
        for (i, s) in buf.iter().enumerate() {
            out[off + i] += s;
        }
    }
    out
}

/// Réverbération légère : trois échos amortis qui donnent de l'air sans empâter.
fn reverb(buf: Vec<f32>, wet: f32) -> Vec<f32> {
    let tail = (0.18 * SR as f32) as usize;
    let mut out = buf.clone();
    out.resize(buf.len() + tail, 0.0);
    for (delay_ms, gain) in [(29u32, 0.38f32), (41, 0.27), (57, 0.18), (83, 0.10)] {
        let d = (delay_ms * SR / 1000) as usize;
        for (i, s) in buf.iter().enumerate() {
            out[i + d] += s * gain * wet;
        }
    }
    out
}

/// Normalise le pic à `peak` et ajoute un fondu de 6 ms en fin de son (pas de clic).
fn finish(mut buf: Vec<f32>, peak: f32) -> Vec<f32> {
    let max = buf.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6);
    let fade = (0.006 * SR as f32) as usize;
    let len = buf.len();
    for (i, s) in buf.iter_mut().enumerate() {
        *s *= peak / max;
        if i + fade > len {
            *s *= (len - i) as f32 / fade as f32;
        }
    }
    buf
}

fn render(sound: Sound) -> Vec<f32> {
    use Sound::*;
    let raw = match sound {
        Move => finish(note(1318.5, 0.07, 0.014, GLASS, 1.0), 0.22),
        Filter => finish(
            mix(vec![(0, note(987.8, 0.12, 0.03, GLASS, 0.8)), (55, note(1318.5, 0.16, 0.04, GLASS, 1.0))]),
            0.30,
        ),
        Select => finish(
            reverb(mix(vec![(0, note(783.99, 0.20, 0.06, GLASS, 0.8)), (65, note(1046.5, 0.30, 0.09, GLASS, 1.0))]), 0.5),
            0.38,
        ),
        Back => finish(
            reverb(mix(vec![(0, note(1046.5, 0.14, 0.04, GLASS, 0.8)), (60, note(783.99, 0.22, 0.07, GLASS, 0.9))]), 0.4),
            0.30,
        ),
        Open => finish(
            reverb(
                mix(vec![
                    (0, note(659.25, 0.18, 0.05, GLASS, 0.7)),
                    (42, note(880.0, 0.18, 0.05, GLASS, 0.85)),
                    (84, note(1318.5, 0.30, 0.09, GLASS, 1.0)),
                ]),
                0.5,
            ),
            0.32,
        ),
        Close => finish(
            reverb(mix(vec![(0, note(1318.5, 0.14, 0.04, GLASS, 0.8)), (48, note(880.0, 0.24, 0.07, GLASS, 0.9))]), 0.4),
            0.26,
        ),
        Launch => {
            // arpège ascendant do-mi-sol-do + nappe grave qui gonfle : le « on y va »
            let pad: Vec<f32> = (0..(0.9 * SR as f32) as usize)
                .map(|i| {
                    let t = i as f32 / SR as f32;
                    let swell = (t / 0.35).min(1.0) * (-((t - 0.35).max(0.0)) / 0.3).exp();
                    ((2.0 * std::f32::consts::PI * 130.81 * t).sin() + 0.5 * (2.0 * std::f32::consts::PI * 196.0 * t).sin()) * swell * 0.35
                })
                .collect();
            finish(
                reverb(
                    mix(vec![
                        (0, pad),
                        (0, note(523.25, 0.35, 0.14, GLASS, 0.8)),
                        (75, note(659.25, 0.35, 0.14, GLASS, 0.85)),
                        (150, note(783.99, 0.40, 0.16, GLASS, 0.9)),
                        (225, note(1046.5, 0.70, 0.30, GLASS, 1.0)),
                    ]),
                    0.7,
                ),
                0.48,
            )
        }
        Favorite => finish(
            reverb(
                mix(vec![
                    (0, note(1568.0, 0.14, 0.04, GLASS, 0.8)),
                    (50, note(2093.0, 0.14, 0.04, GLASS, 0.85)),
                    (100, note(2637.0, 0.30, 0.08, GLASS, 0.9)),
                ]),
                0.5,
            ),
            0.28,
        ),
        Key => finish(note(1046.5, 0.05, 0.010, SOFT, 1.0), 0.18),
        Toast => finish(reverb(note(880.0, 0.20, 0.06, GLASS, 1.0), 0.4), 0.22),
        Error => finish(mix(vec![(0, note(220.0, 0.22, 0.07, SOFT, 1.0)), (70, note(185.0, 0.30, 0.10, SOFT, 1.0))]), 0.32),
        Start => finish(
            reverb(
                mix(vec![
                    (0, note(523.25, 0.5, 0.20, GLASS, 0.7)),
                    (110, note(659.25, 0.5, 0.20, GLASS, 0.8)),
                    (220, note(783.99, 0.5, 0.22, GLASS, 0.9)),
                    (330, note(987.77, 0.9, 0.34, GLASS, 1.0)),
                ]),
                0.8,
            ),
            0.34,
        ),
    };
    raw
}

/// Tous les sons rendus en mémoire (pour les tests et l'aperçu WAV).
pub fn render_all() -> Vec<(Sound, Vec<f32>)> {
    use Sound::*;
    [Move, Filter, Select, Back, Open, Close, Launch, Favorite, Key, Toast, Error, Start].into_iter().map(|s| (s, render(s))).collect()
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
pub fn init(enabled: bool, volume: f32) -> bool {
    let (tx, rx) = channel::<Sound>();
    let player = Player { tx: Mutex::new(tx), enabled: AtomicBool::new(enabled), volume: AtomicU32::new((volume.clamp(0.0, 1.0) * 1000.0) as u32) };
    if PLAYER.set(player).is_err() {
        return true;
    }
    let (ready_tx, ready_rx) = channel::<bool>();
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
        let buffers: HashMap<Sound, Vec<f32>> = render_all().into_iter().collect();
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

    #[test]
    fn every_sound_is_finite_quiet_enough_and_click_free() {
        for (sound, buf) in render_all() {
            assert!(!buf.is_empty(), "{sound:?} vide");
            assert!(buf.iter().all(|s| s.is_finite()), "{sound:?} contient NaN/inf");
            let peak = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!((0.05..=0.6).contains(&peak), "{sound:?} : pic {peak} hors de la plage douce");
            assert!(buf.last().unwrap().abs() < 0.01, "{sound:?} ne finit pas à zéro (clic)");
            assert!(buf[0].abs() < 0.01, "{sound:?} ne commence pas à zéro (clic)");
            let secs = buf.len() as f32 / SR as f32;
            assert!(secs < 1.6, "{sound:?} trop long : {secs} s");
        }
    }

    #[test]
    fn short_ui_sounds_stay_short() {
        let get = |s: Sound| render(s).len() as f32 / SR as f32;
        assert!(get(Sound::Move) < 0.15);
        assert!(get(Sound::Key) < 0.12);
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
