//! Prépare les échantillons embarqués : décode, passe en mono 44,1 kHz, retire le silence, normalise (≤ 4 s)
//! et écrit un WAV 16 bits dans `sound/assets`.
//! cargo run --release -p emunest-sound --example trim -- <source> <sortie.wav>
use emunest_sound::{bank, write_wav};

fn main() {
    let mut a = std::env::args().skip(1);
    let (src, dst) = (a.next().expect("source"), a.next().expect("sortie"));
    let s = bank::load(std::path::Path::new(&src)).expect("lecture");
    write_wav(std::path::Path::new(&dst), &s.data).expect("écriture");
    println!("{dst} : {:.2} s", s.data.len() as f32 / bank::SR as f32);
}