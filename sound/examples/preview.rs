//! Écrit un WAV par son dans le dossier donné (écoute hors application) :
//! cargo run -p emunest-sound --example preview -- D:\chemin\sons
fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "sons".into()));
    std::fs::create_dir_all(&dir).unwrap();
    // 2e argument facultatif : dossier avec bell.* / chime.* (échantillons de l'utilisateur)
    let bank = std::env::args().nth(2).map(|d| emunest_sound::load_bank(std::path::Path::new(&d))).unwrap_or_default();
    for (sound, samples) in emunest_sound::render_all_with(&bank) {
        let path = dir.join(format!("{sound:?}.wav").to_lowercase());
        emunest_sound::write_wav(&path, &samples).unwrap();
        println!("{} ({:.2} s)", path.display(), samples.len() as f32 / 44_100.0);
    }
}
