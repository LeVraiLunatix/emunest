//! Analyse des échantillons d'un dossier (hauteur, brillance, durée) pour choisir des bells chauds :
//! cargo run --release -p emunest-sound --example analyze -- "D:\chemin\vers\les\samples"
use emunest_sound::bank;

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("dossier"));
    let mut rows = Vec::new();
    for e in walk(&dir) {
        match bank::load(&e) {
            Ok(s) => {
                let secs = s.data.len() as f32 / bank::SR as f32;
                rows.push((bank::spectral_centroid(&s.data), s.base_freq, s.confidence, secs, e));
            }
            Err(err) => eprintln!("ignoré : {err:#}"),
        }
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    println!("centroide(Hz)  hauteur(Hz)  confiance  durée(s)  fichier");
    for (c, f, conf, secs, p) in rows {
        println!("{c:>10.0}  {f:>10.1}  {conf:>8.2}  {secs:>7.2}  {}", p.file_name().unwrap().to_string_lossy());
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| ["wav", "ogg", "flac", "mp3"].contains(&x.to_lowercase().as_str())) {
            out.push(p);
        }
    }
    out
}
