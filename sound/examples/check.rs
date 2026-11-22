//! Vérifie la chaîne audio sans bruit : ouvre le périphérique et « joue » tous les sons à volume zéro.
fn main() {
    let ok = emunest_sound::init(true, 0.0, None);
    println!("périphérique audio ouvert : {ok}");
    for (s, _) in emunest_sound::render_all() {
        emunest_sound::play(s);
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    println!("tous les sons ont été envoyés");
}
