//! Banque d'échantillons de l'utilisateur : des « bells » (one-shots) choisis par lui, rejoués à la hauteur voulue.
//! Rien n'est embarqué : les fichiers restent dans `<données>/sounds` sur sa machine (leurs licences ne sont pas les nôtres).
use anyhow::{bail, Context, Result};
use rodio::Source;
use std::path::Path;

pub const SR: u32 = 44_100;

/// Un échantillon mono à 44,1 kHz, normalisé, avec sa hauteur détectée.
#[derive(Clone, Debug)]
pub struct Sample {
    pub data: Vec<f32>,
    /// fréquence fondamentale estimée (Hz)
    pub base_freq: f32,
    /// confiance de la détection (0..1) ; basse = son peu « chantant »
    pub confidence: f32,
}

/// Comme `load`, avec la hauteur cherchée entre 25 et 150 Hz (basses, 808).
pub fn load_bass(path: &Path) -> Result<Sample> {
    let mut s = load(path)?;
    (s.base_freq, s.confidence) = detect_pitch_in(&s.data, 25.0, 150.0);
    Ok(s)
}

/// Décode un fichier audio (wav, ogg, flac, mp3), le passe en mono 44,1 kHz, retire le silence du début
/// et normalise le pic à 1. Limité à 4 s : on n'a besoin que de l'attaque et de la première résonance.
pub fn load(path: &Path) -> Result<Sample> {
    let file = std::fs::File::open(path).with_context(|| format!("ouverture de {}", path.display()))?;
    let decoder = rodio::Decoder::try_from(file).with_context(|| format!("format audio non reconnu : {}", path.display()))?;
    let channels = decoder.channels().get() as usize;
    let rate = decoder.sample_rate().get() as f32;
    let raw: Vec<f32> = decoder.collect();
    if raw.is_empty() {
        bail!("fichier vide : {}", path.display());
    }
    let mono: Vec<f32> = raw.chunks(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect();

    // rééchantillonnage linéaire vers 44,1 kHz
    let ratio = rate / SR as f32;
    let out_len = (mono.len() as f32 / ratio) as usize;
    let mut data: Vec<f32> = (0..out_len)
        .map(|i| {
            let pos = i as f32 * ratio;
            let (i0, frac) = (pos as usize, pos.fract());
            let a = mono[i0.min(mono.len() - 1)];
            let b = mono[(i0 + 1).min(mono.len() - 1)];
            a + (b - a) * frac
        })
        .collect();

    // silence de départ (< 2 % du pic) retiré, durée limitée
    let peak = data.iter().fold(0.0f32, |m, s| m.max(s.abs())).max(1e-6);
    let start = data.iter().position(|s| s.abs() > peak * 0.02).unwrap_or(0);
    data.drain(..start);
    data.truncate(SR as usize * 4);
    for s in data.iter_mut() {
        *s /= peak;
    }
    let (base_freq, confidence) = detect_pitch(&data);
    Ok(Sample { data, base_freq, confidence })
}

/// Hauteur par autocorrélation normalisée, sur une fenêtre juste après l'attaque.
pub fn detect_pitch(data: &[f32]) -> (f32, f32) {
    detect_pitch_in(data, 70.0, 2000.0)
}

/// Pareil, dans la plage `fmin..fmax` Hz (les 808 descendent sous 70 Hz).
pub fn detect_pitch_in(data: &[f32], fmin: f32, fmax: f32) -> (f32, f32) {
    let sr = SR as f32;
    let (lo, hi) = ((sr / fmax) as usize, (sr / fmin) as usize);
    let win = 8192usize;
    let from = (0.08 * sr) as usize;
    if data.len() < from + win + hi + 2 {
        return (440.0, 0.0);
    }
    let seg = &data[from..from + win + hi];
    let energy = |a: usize, b: usize| seg[a..b].iter().map(|s| s * s).sum::<f32>();
    let norm0 = energy(0, win).max(1e-9);
    let mut best = (0usize, 0.0f32);
    let mut corr = vec![0.0f32; hi + 2];
    for lag in lo..=hi {
        let c: f32 = (0..win).map(|i| seg[i] * seg[i + lag]).sum();
        corr[lag] = c / (norm0.sqrt() * energy(lag, lag + win).max(1e-9).sqrt());
        if corr[lag] > best.1 {
            best = (lag, corr[lag]);
        }
    }
    // on préfère le plus petit retard dont la corrélation est proche du maximum (évite les sous-harmoniques)
    let lag = (lo..=hi).find(|&l| corr[l] > best.1 * 0.92 && corr[l] >= corr[l - 1] && corr[l] >= corr[l + 1]).unwrap_or(best.0);
    // interpolation parabolique autour du pic
    let (a, b, c) = (corr[lag - 1], corr[lag], corr[lag + 1]);
    let denom = a - 2.0 * b + c;
    let shift = if denom.abs() > 1e-9 { 0.5 * (a - c) / denom } else { 0.0 };
    (sr / (lag as f32 + shift), best.1.clamp(0.0, 1.0))
}

/// FFT radix-2 récursive minimale (analyse hors ligne).
fn fft(buf: &mut [(f32, f32)]) {
    let n = buf.len();
    if n <= 1 {
        return;
    }
    let mut even: Vec<_> = buf.iter().step_by(2).copied().collect();
    let mut odd: Vec<_> = buf.iter().skip(1).step_by(2).copied().collect();
    fft(&mut even);
    fft(&mut odd);
    for k in 0..n / 2 {
        let ang = -2.0 * std::f32::consts::PI * k as f32 / n as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        let (or, oi) = (odd[k].0 * wr - odd[k].1 * wi, odd[k].0 * wi + odd[k].1 * wr);
        buf[k] = (even[k].0 + or, even[k].1 + oi);
        buf[k + n / 2] = (even[k].0 - or, even[k].1 - oi);
    }
}

/// Centroïde spectral (Hz) d'une fenêtre juste après l'attaque : plus il est bas, plus le son est chaud.
pub fn spectral_centroid(data: &[f32]) -> f32 {
    let n = 4096;
    let from = (0.02 * SR as f32) as usize;
    if data.len() < from + n {
        return 0.0;
    }
    let mut buf: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let hann = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos();
            (data[from + i] * hann, 0.0)
        })
        .collect();
    fft(&mut buf);
    let (mut num, mut den) = (0.0f32, 1e-9f32);
    for (k, (re, im)) in buf.iter().take(n / 2).enumerate() {
        let mag = (re * re + im * im).sqrt();
        num += mag * k as f32 * SR as f32 / n as f32;
        den += mag;
    }
    num / den
}

impl Sample {
    /// Joue l'échantillon à la hauteur `freq` pendant `secs`, avec une décroissance supplémentaire `decay`
    /// (secondes) qui raccourcit la résonance, une attaque douce de `attack` s et un fondu final.
    pub fn note(&self, freq: f32, secs: f32, attack: f32, decay: f32, amp: f32) -> Vec<f32> {
        let ratio = (freq / self.base_freq).clamp(0.25, 4.0); // vitesse de lecture = changement de hauteur
        let total = (secs * SR as f32) as usize;
        (0..total)
            .map(|i| {
                let pos = i as f32 * ratio;
                let (i0, frac) = (pos as usize, pos.fract());
                let a = self.data.get(i0).copied().unwrap_or(0.0);
                let b = self.data.get(i0 + 1).copied().unwrap_or(0.0);
                let t = i as f32 / SR as f32;
                let att = (t / attack.max(0.001)).min(1.0);
                let env = att * att * (3.0 - 2.0 * att) * (-t / decay).exp();
                let tail = ((total - i) as f32 / (0.03 * SR as f32)).min(1.0);
                (a + (b - a) * frac) * env * tail * amp
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Écrit un WAV mono 16 bits : sinus qui s'éteint, comme un bell simple.
    fn write_bell(path: &Path, freq: f32, rate: u32) {
        let samples: Vec<f32> = (0..(rate as f32 * 1.5) as usize)
            .map(|i| {
                let t = i as f32 / rate as f32;
                (2.0 * std::f32::consts::PI * freq * t).sin() * (-t / 0.4).exp() * 0.8 + (2.0 * std::f32::consts::PI * freq * 2.76 * t).sin() * (-t / 0.2).exp() * 0.2
            })
            .collect();
        let data: Vec<u8> = samples.iter().flat_map(|s| ((s * 32767.0) as i16).to_le_bytes()).collect();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn loads_resamples_and_finds_the_pitch() {
        let dir = std::env::temp_dir().join(format!("emunest-bank-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for rate in [44_100u32, 48_000] {
            let p = dir.join(format!("bell{rate}.wav"));
            write_bell(&p, 660.0, rate);
            let s = load(&p).unwrap();
            assert!((s.base_freq - 660.0).abs() < 660.0 * 0.02, "hauteur {} Hz pour 660 Hz ({rate})", s.base_freq);
            assert!(s.confidence > 0.8);
            assert!((s.data.iter().fold(0.0f32, |m, x| m.max(x.abs())) - 1.0).abs() < 1e-3, "normalisé");
        }
        // jouée une octave plus haut : deux fois plus rapide donc la moitié de la durée utile, toujours finie et sans clic
        let s = load(&dir.join("bell44100.wav")).unwrap();
        let n = s.note(1320.0, 0.5, 0.01, 0.3, 0.9);
        assert_eq!(n.len(), (0.5 * SR as f32) as usize);
        assert!(n.iter().all(|x| x.is_finite()) && n.last().unwrap().abs() < 0.01 && n[0].abs() < 0.01);
        assert!(spectral_centroid(&s.data) < 2500.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
