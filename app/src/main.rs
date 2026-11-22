mod pad;

use anyhow::Result;
use emunest_core::install::Phase;
use emunest_core::{install, launch, Game, Library};
use emunest_emulators::Profile;
use emunest_ui::{Answer, ComponentHandle, MainWindow, MenuAction, Weak};
use notify::{RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn toast(ui: &Weak<MainWindow>, msg: String) {
    let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::toast(&w, &msg));
}

fn profile_of(name: &str) -> Option<Profile> {
    emunest_emulators::builtin_profiles().ok()?.into_iter().find(|p| p.name == name)
}

/// Relit la bibliothèque et rafraîchit l'écran (garde filtre, recherche et sélection).
fn reload(data: &Path, ui: &Weak<MainWindow>) {
    match Library::open(&data.join("library.db")).and_then(|l| l.games()) {
        Ok(games) => {
            let d = data.to_path_buf();
            let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::set_games(&w, &games, &d));
        }
        Err(e) => tracing::error!("bibliothèque : {e:#}"),
    }
}

/// Télécharge les jaquettes qui manquent ; chacune apparaît dès qu'elle est prête.
fn fetch_covers(data: &Path, ui: &Weak<MainWindow>, games: &[Game]) {
    emunest_metadata::fetch_missing(data, games, &mut |g| {
        let (g, d) = (g.clone(), data.to_path_buf());
        let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::refresh_cover(&w, &g, &d));
    });
}

/// Scanne le dossier de ROMs, met la bibliothèque à jour puis cherche les jaquettes des nouveaux jeux.
fn rescan(data: &Path, ui: &Weak<MainWindow>, dir: &Path) -> Result<()> {
    let mut lib = Library::open(&data.join("library.db"))?;
    let n = lib.scan(dir, &emunest_emulators::builtin_profiles()?)?;
    tracing::info!("{n} jeux trouvés dans {}", dir.display());
    let games = lib.games()?;
    drop(lib);
    reload(data, ui);
    fetch_covers(data, ui, &games);
    Ok(())
}

/// Surveille le dossier de ROMs : après un moment de calme (copie terminée), on rescanne tout seul.
fn watch_roms(data: PathBuf, ui: Weak<MainWindow>, dir: PathBuf) {
    let last_event = Arc::new(AtomicU64::new(0));
    let stamp = || SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let l = last_event.clone();
    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            l.store(stamp(), Ordering::SeqCst);
        }
    }) {
        Ok(w) => w,
        Err(e) => return tracing::warn!("surveillance du dossier impossible : {e}"),
    };
    if let Err(e) = watcher.watch(&dir, RecursiveMode::Recursive) {
        return tracing::warn!("surveillance de {} impossible : {e}", dir.display());
    }
    std::thread::spawn(move || {
        let _keep = watcher; // la surveillance s'arrête avec ce thread
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let t = last_event.load(Ordering::SeqCst);
            // un changement, puis 4 s sans nouvel événement : la copie des gros fichiers est finie
            if t != 0 && stamp().saturating_sub(t) > 4000 && last_event.compare_exchange(t, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                if let Err(e) = rescan(&data, &ui, &dir) {
                    tracing::warn!("rescan : {e:#}");
                }
            }
        }
    });
}

/// Installe (ou met à jour) l'émulateur en affichant la progression.
fn run_install(data: &Path, ui: &Weak<MainWindow>, profile: &Profile) -> Result<PathBuf> {
    let title = format!("Installation de {}", profile.name);
    let exe = install::install(data, profile, &mut |phase, f| {
        let (text, frac) = match phase {
            Phase::Resolving => ("Recherche de la dernière version…".to_string(), 0.0),
            Phase::Downloading => (format!("Téléchargement… {} %", (f * 100.0) as u32), f),
            Phase::Extracting => ("Extraction des fichiers…".to_string(), 1.0),
        };
        let title = title.clone();
        let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::show_progress(&w, &title, &text, frac));
    });
    let _ = ui.upgrade_in_event_loop(|w| emunest_ui::close_dialog(&w));
    exe
}

/// Propose d'installer l'émulateur manquant puis l'installe.
fn offer_install(data: &Path, ui: &Weak<MainWindow>, profile: &Profile) -> Result<Option<PathBuf>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let title = format!("{} n'est pas installé", profile.name);
    ui.upgrade_in_event_loop(move |w| {
        emunest_ui::ask(&w, &title, "Télécharger la dernière version et l'installer dans EmuNest ?", false, tx)
    })?;
    if rx.recv() != Ok(Answer::Yes) {
        return Ok(None);
    }
    run_install(data, ui, profile).map(Some)
}

/// Écran « fichier manquant » : tant qu'un BIOS / une clé exigé(e) n'est pas là, explique où le mettre
/// (l'utilisateur le fournit lui-même, EmuNest ne télécharge rien). Renvoie faux si l'utilisateur renonce.
fn ensure_files(ui: &Weak<MainWindow>, exe: &Path, profile: &Profile) -> bool {
    let mut retry = false;
    loop {
        let missing = launch::missing_files(exe, profile);
        let Some(req) = missing.first() else {
            let _ = ui.upgrade_in_event_loop(|w| emunest_ui::close_dialog(&w));
            return true;
        };
        let dir = launch::requirement_dir(exe, req);
        let _ = std::fs::create_dir_all(&dir);
        let mut text = format!("{}\nCopie ce fichier, issu de ta propre console, dans :\n{}", req.hint, dir.display());
        if retry {
            text.push_str("\nToujours introuvable : vérifie l'emplacement et la taille du fichier.");
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let title = format!("{} manquant", req.label);
        let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::ask(&w, &title, &text, true, tx));
        match rx.recv() {
            Ok(Answer::Yes) => retry = true,
            Ok(Answer::Extra) => {
                let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                retry = false;
            }
            _ => return false,
        }
    }
}

/// Réduit la fenêtre pendant que `child` tourne, puis la rend. Renvoie la durée de la partie.
fn wait_minimized(ui: &Weak<MainWindow>, child: &mut std::process::Child) -> Duration {
    let _ = ui.upgrade_in_event_loop(|w| w.window().set_minimized(true));
    let start = Instant::now();
    let _ = child.wait();
    let _ = ui.upgrade_in_event_loop(|w| w.window().set_minimized(false));
    start.elapsed()
}

/// Lance `game` et attend la fin de l'émulateur. Tourne hors du thread UI : la fenêtre est réduite
/// pendant la partie et revient toute seule ; le temps de jeu est enregistré.
fn play(data: &Path, ui: &Weak<MainWindow>, game: Game) {
    let Some(profile) = profile_of(&game.emulator) else {
        return toast(ui, format!("Profil d'émulateur « {} » introuvable", game.emulator));
    };
    let exe = match launch::find_executable(data, &profile) {
        Some(e) => e,
        None => match offer_install(data, ui, &profile) {
            Ok(Some(e)) => e,
            Ok(None) => return, // refusé
            Err(e) => {
                let _ = ui.upgrade_in_event_loop(|w| emunest_ui::close_dialog(&w));
                return toast(ui, format!("Installation de {} impossible : {e:#}", profile.name));
            }
        },
    };
    if !ensure_files(ui, &exe, &profile) {
        return;
    }
    let mut child = match launch::launch(&exe, &profile, &game.path) {
        Ok(c) => c,
        Err(e) => return toast(ui, format!("{e:#}")),
    };
    toast(ui, format!("{} — lancé avec {}", game.title, profile.name));
    let played = wait_minimized(ui, &mut child);
    tracing::info!("{} terminé après {played:?}", profile.name);
    if let Ok(lib) = Library::open(&data.join("library.db")) {
        let _ = lib.record_play(&game.path, played.as_secs() as i64);
    }
    reload(data, ui);
}

/// Actions du menu Options qui touchent au système (hors worker UI).
fn menu_action(data: &Path, ui: &Weak<MainWindow>, action: MenuAction, game: Game) {
    let profile = profile_of(&game.emulator);
    match action {
        MenuAction::OpenEmulator => {
            let Some(profile) = profile else { return toast(ui, "Émulateur inconnu".into()) };
            let Some(exe) = launch::find_executable(data, &profile) else {
                return toast(ui, format!("{} n'est pas installé : lance un jeu pour l'installer", profile.name));
            };
            match std::process::Command::new(&exe).current_dir(exe.parent().unwrap_or(Path::new("."))).spawn() {
                Ok(mut child) => {
                    toast(ui, format!("{} ouvert : règle-le puis ferme-le pour revenir", profile.name));
                    wait_minimized(ui, &mut child);
                }
                Err(e) => toast(ui, format!("Impossible d'ouvrir {} : {e}", profile.name)),
            }
        }
        MenuAction::UpdateEmulator => {
            let Some(profile) = profile else { return toast(ui, "Émulateur inconnu".into()) };
            if matches!(profile.source, emunest_emulators::Source::Manual) {
                return toast(ui, format!("{} se met à jour à la main (pas d'installation automatique)", profile.name));
            }
            if let Some(exe) = launch::find_executable(data, &profile) {
                if !exe.starts_with(launch::managed_dir(data, &profile)) {
                    return toast(ui, format!("{} est installé hors d'EmuNest : mets-le à jour toi-même", profile.name));
                }
            }
            match run_install(data, ui, &profile) {
                Ok(_) => toast(ui, format!("{} est à jour", profile.name)),
                Err(e) => toast(ui, format!("Mise à jour de {} impossible : {e:#}", profile.name)),
            }
        }
        MenuAction::OpenFolder => {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", game.path.display())).spawn();
        }
        MenuAction::ChooseCover => {
            let picked = rfd::FileDialog::new().set_title("Choisir une jaquette").add_filter("Images", &["png", "jpg", "jpeg"]).pick_file();
            let Some(src) = picked else { return };
            match emunest_metadata::set_custom_cover(data, &game, &src) {
                Ok(()) => {
                    let (g, d) = (game.clone(), data.to_path_buf());
                    let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::refresh_cover(&w, &g, &d));
                    toast(ui, "Jaquette remplacée".into());
                }
                Err(e) => toast(ui, format!("{e:#}")),
            }
        }
        MenuAction::RefetchCover => {
            emunest_metadata::reset_cover(data, &game);
            toast(ui, "Nouvelle recherche de la jaquette…".into());
            fetch_covers(data, ui, std::slice::from_ref(&game));
            let (g, d) = (game.clone(), data.to_path_buf());
            let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::refresh_cover(&w, &g, &d));
        }
        MenuAction::Play | MenuAction::ToggleFavorite => {} // gérés par l'interface
    }
}

/// Dossier de données : `~/.emunest`. Surtout pas `%LOCALAPPDATA%` : une app lancée depuis un programme
/// MSIX (comme le bureau Claude) y voit une copie virtualisée, donc une config et une bibliothèque vides.
fn data_dir() -> PathBuf {
    // EMUNEST_DATA : dossier de données alternatif (tests, installation portable)
    if let Some(d) = std::env::var_os("EMUNEST_DATA") {
        return d.into();
    }
    let var = |k: &str| std::env::var(k).ok().map(PathBuf::from);
    let new = var("USERPROFILE").or_else(|| var("HOME")).unwrap_or_else(|| ".".into()).join(".emunest");
    if !new.exists() {
        // reprise de l'ancien emplacement (versions précédentes)
        if let Some(old) = var("LOCALAPPDATA").map(|p| p.join("emunest")).filter(|p| p.is_dir()) {
            let _ = std::fs::rename(old, &new);
        }
    }
    new
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").try_init().ok();

    // usage : emunest [--windowed] [--pad=ps|xbox|nintendo] [dossier_roms]
    let mut windowed = false;
    let mut forced_pad = None;
    let mut roms: Option<PathBuf> = None;
    for a in std::env::args().skip(1) {
        if a == "--windowed" {
            windowed = true
        } else if let Some(kind) = a.strip_prefix("--pad=") {
            // aperçu des symboles sans manette
            forced_pad = match kind {
                "ps" => Some(emunest_ui::PadKind::PlayStation),
                "nintendo" => Some(emunest_ui::PadKind::Nintendo),
                _ => Some(emunest_ui::PadKind::Xbox),
            };
        } else {
            roms = Some(a.into())
        }
    }

    let data = data_dir();
    let win = emunest_ui::create(!windowed)?;
    if let Some(kind) = forced_pad {
        emunest_ui::set_pad_kind(&win, kind);
        win.set_pad_active(true);
    }
    let weak = win.as_weak();

    // Une seule action longue à la fois (partie, installation…) : Entrée pendant le jeu ne relance rien.
    let busy = Arc::new(AtomicBool::new(false));
    if forced_pad.is_none() {
        pad::spawn(weak.clone(), busy.clone());
    }
    let run_busy = {
        let (data, weak, busy) = (data.clone(), weak.clone(), busy.clone());
        move |job: Box<dyn FnOnce(&Path, &Weak<MainWindow>) + Send>| {
            if busy.swap(true, Ordering::SeqCst) {
                return;
            }
            let (data, weak, busy) = (data.clone(), weak.clone(), busy.clone());
            std::thread::spawn(move || {
                job(&data, &weak);
                busy.store(false, Ordering::SeqCst);
            });
        }
    };
    let run_busy = Arc::new(run_busy);

    let rb = run_busy.clone();
    emunest_ui::on_launch(&win, move |game| rb(Box::new(move |d, ui| play(d, ui, game))));
    let rb = run_busy.clone();
    emunest_ui::on_menu(move |action, game| rb(Box::new(move |d, ui| menu_action(d, ui, action, game))));
    {
        let (data, weak) = (data.clone(), weak.clone());
        emunest_ui::on_favorite(move |game| {
            let (data, weak) = (data.clone(), weak.clone());
            std::thread::spawn(move || {
                if let Ok(lib) = Library::open(&data.join("library.db")) {
                    let _ = lib.set_favorite(&game.path, !game.favorite);
                }
                reload(&data, &weak);
            });
        });
    }

    // Tout le travail lent (scan, réseau) hors du thread UI : la fenêtre s'affiche tout de suite.
    std::thread::spawn(move || {
        reload(&data, &weak); // la bibliothèque déjà connue s'affiche sans attendre le scan
        if let Some(dir) = roms.or_else(|| launch::roms_dir(&data)) {
            if let Err(e) = rescan(&data, &weak, &dir) {
                tracing::error!("scan : {e:#}");
            }
            watch_roms(data, weak, dir);
        } else if let Ok(games) = Library::open(&data.join("library.db")).and_then(|l| l.games()) {
            fetch_covers(&data, &weak, &games);
        }
    });

    win.run()?;
    Ok(())
}
