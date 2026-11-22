mod pad;

use anyhow::Result;
use emunest_core::config::{self, Config};
use emunest_core::install::Phase;
use emunest_core::{install, launch, Game, Library};
use emunest_emulators::Profile;
use emunest_ui::{Answer, ComponentHandle, MainWindow, MenuAction, Weak};
use notify::{RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Ui = Weak<MainWindow>;

fn toast(ui: &Ui, msg: impl Into<String>) {
    let msg = msg.into();
    let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::toast(&w, &msg));
}

fn profile_of(name: &str) -> Option<Profile> {
    emunest_emulators::builtin_profiles().ok()?.into_iter().find(|p| p.name == name)
}

fn db(data: &Path) -> Result<Library> {
    Library::open(&data.join("library.db"))
}

/// Dossiers de ROMs à scanner : l'argument de la ligne de commande, sinon ceux de config.toml.
fn rom_dirs(data: &Path, cli: &Option<PathBuf>) -> Vec<PathBuf> {
    cli.clone().map(|d| vec![d]).unwrap_or_else(|| config::load(data).roms)
}

/// Relit la bibliothèque et rafraîchit l'écran (garde filtre, recherche et sélection).
fn reload(data: &Path, ui: &Ui) {
    match db(data).and_then(|l| Ok((l.games()?, l.collections()?))) {
        Ok((games, collections)) => {
            let d = data.to_path_buf();
            let _ = ui.upgrade_in_event_loop(move |w| {
                emunest_ui::set_collections(collections);
                emunest_ui::set_games(&w, &games, &d);
            });
        }
        Err(e) => tracing::error!("bibliothèque : {e:#}"),
    }
}

/// Télécharge les jaquettes qui manquent ; chacune apparaît dès qu'elle est prête.
fn fetch_covers(data: &Path, ui: &Ui, games: &[Game]) {
    emunest_metadata::fetch_missing(data, games, &mut |g| {
        let (g, d) = (g.clone(), data.to_path_buf());
        let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::refresh_cover(&w, &g, &d));
    });
}

/// Scanne les dossiers de ROMs, met la bibliothèque à jour puis cherche les jaquettes des nouveaux jeux.
fn rescan(data: &Path, ui: &Ui, dirs: &[PathBuf]) -> Result<()> {
    let mut lib = db(data)?;
    let profiles = emunest_emulators::builtin_profiles()?;
    for dir in dirs {
        let n = lib.scan(dir, &profiles)?;
        tracing::info!("{n} jeux trouvés dans {}", dir.display());
    }
    let games = lib.games()?;
    drop(lib);
    reload(data, ui);
    fetch_covers(data, ui, &games);
    Ok(())
}

/// Surveille un dossier de ROMs : après un moment de calme (copie terminée), on rescanne tout seul.
fn watch_roms(data: PathBuf, ui: Ui, dir: PathBuf, cli: Option<PathBuf>) {
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
                if let Err(e) = rescan(&data, &ui, &rom_dirs(&data, &cli)) {
                    tracing::warn!("rescan : {e:#}");
                }
            }
        }
    });
}

/// Installe (ou met à jour) l'émulateur en affichant la progression.
fn run_install(data: &Path, ui: &Ui, profile: &Profile) -> Result<PathBuf> {
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
fn offer_install(data: &Path, ui: &Ui, profile: &Profile) -> Result<Option<PathBuf>> {
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
fn ensure_files(ui: &Ui, exe: &Path, profile: &Profile) -> bool {
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
fn wait_minimized(ui: &Ui, child: &mut std::process::Child) -> Duration {
    let _ = ui.upgrade_in_event_loop(|w| w.window().set_minimized(true));
    let start = Instant::now();
    let _ = child.wait();
    let _ = ui.upgrade_in_event_loop(|w| w.window().set_minimized(false));
    start.elapsed()
}

/// Lance `game` et attend la fin de l'émulateur. Tourne hors du thread UI : la fenêtre est réduite
/// pendant la partie et revient toute seule ; le temps de jeu est enregistré.
fn play(data: &Path, ui: &Ui, game: Game) {
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
    let mut child = match launch::launch(&exe, &profile, &game.path, &game.extra_args) {
        Ok(c) => c,
        Err(e) => return toast(ui, format!("{e:#}")),
    };
    toast(ui, format!("{} — lancé avec {}", game.title, profile.name));
    let played = wait_minimized(ui, &mut child);
    tracing::info!("{} terminé après {played:?}", profile.name);
    if let Ok(lib) = db(data) {
        let _ = lib.record_play(&game.path, played.as_secs() as i64);
    }
    reload(data, ui);
}

// ---------------------------------------------------------------------------------------------
// Menus : le menu Options d'un jeu et les sous-menus (collections, réglages du jeu, jaquette, réglages)
// ---------------------------------------------------------------------------------------------

/// Sens d'une ligne de sous-menu (les lignes sont repérées par leur numéro côté interface).
#[derive(Clone, Debug)]
enum Sub {
    ToggleCollection(String),
    NewCollection,
    DeleteCollectionList,
    DeleteCollection(String),
    PickEmulator,
    SetEmulator(Option<String>),
    EditArgs,
    ResetGame,
    ChooseCover,
    RefetchCover,
    RomFolders,
    AddRomFolder,
    RemoveRomFolder(PathBuf),
    Emulators,
    InstallEmulator(String),
    OpenData,
    OpenConfig,
}

static SUB: Mutex<Vec<Sub>> = Mutex::new(Vec::new());

fn show_sub(ui: &Ui, title: String, entries: Vec<(Sub, String)>) {
    let mut subs = SUB.lock().unwrap();
    *subs = entries.iter().map(|(s, _)| s.clone()).collect();
    let items: Vec<(MenuAction, String)> =
        entries.into_iter().enumerate().map(|(i, (_, label))| (MenuAction::Item(i as u32), label)).collect();
    drop(subs);
    let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::show_menu(&w, &title, items));
}

fn prompt(ui: &Ui, title: &str, initial: &str) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (title, initial) = (title.to_string(), initial.to_string());
    ui.upgrade_in_event_loop(move |w| emunest_ui::ask_text(&w, &title, &initial, tx)).ok()?;
    rx.recv().ok().flatten().map(|t| t.trim().to_string())
}

fn explorer_select(path: &Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
}

fn open_with_default(path: &Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("cmd").raw_arg(format!("/C start \"\" \"{}\"", path.display())).spawn();
}

fn show_collections(data: &Path, ui: &Ui, game: &Game) {
    let names = db(data).and_then(|l| l.collections()).unwrap_or_default();
    let mut entries: Vec<(Sub, String)> = names
        .iter()
        .map(|n| {
            let label = if game.collections.contains(n) { format!("Retirer de « {n} »") } else { format!("Ajouter à « {n} »") };
            (Sub::ToggleCollection(n.clone()), label)
        })
        .collect();
    entries.push((Sub::NewCollection, "Nouvelle collection…".into()));
    if !names.is_empty() {
        entries.push((Sub::DeleteCollectionList, "Supprimer une collection…".into()));
    }
    show_sub(ui, format!("Collections · {}", game.title), entries);
}

fn show_game_settings(ui: &Ui, game: &Game) {
    let emu = match &game.emu_override {
        Some(e) => format!("{e} (choisi)"),
        None => format!("{} (par défaut)", game.emulator),
    };
    let args = if game.extra_args.is_empty() { "aucun".to_string() } else { game.extra_args.clone() };
    show_sub(
        ui,
        format!("Réglages · {}", game.title),
        vec![
            (Sub::PickEmulator, format!("Émulateur : {emu}")),
            (Sub::EditArgs, format!("Arguments supplémentaires : {args}")),
            (Sub::ResetGame, "Réinitialiser les réglages de ce jeu".into()),
        ],
    );
}

fn show_settings(data: &Path, ui: &Ui) {
    let cfg = config::load(data);
    show_sub(
        ui,
        "Réglages d'EmuNest".into(),
        vec![
            (Sub::RomFolders, format!("Dossiers de ROMs ({})…", cfg.roms.len())),
            (Sub::Emulators, "Émulateurs…".into()),
            (Sub::OpenData, "Ouvrir le dossier de données".into()),
            (Sub::OpenConfig, "Ouvrir config.toml".into()),
        ],
    );
}

fn show_rom_folders(data: &Path, ui: &Ui) {
    let cfg = config::load(data);
    let mut entries: Vec<(Sub, String)> =
        cfg.roms.iter().map(|d| (Sub::RemoveRomFolder(d.clone()), format!("Retirer : {}", d.display()))).collect();
    entries.push((Sub::AddRomFolder, "Ajouter un dossier de ROMs…".into()));
    show_sub(ui, "Dossiers de ROMs".into(), entries);
}

fn show_emulators(data: &Path, ui: &Ui) {
    let entries = emunest_emulators::builtin_profiles()
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let state = match launch::find_executable(data, &p) {
                Some(e) if e.starts_with(launch::managed_dir(data, &p)) => "installé par EmuNest · mettre à jour",
                Some(_) => "installé ailleurs",
                None if matches!(p.source, emunest_emulators::Source::Manual) => "à installer toi-même",
                None => "non installé · installer",
            };
            (Sub::InstallEmulator(p.name.clone()), format!("{} — {state}", p.name))
        })
        .collect();
    show_sub(ui, "Émulateurs".into(), entries);
}

/// Met à jour (ou installe) un émulateur géré par EmuNest.
fn update_emulator(data: &Path, ui: &Ui, profile: &Profile) {
    if matches!(profile.source, emunest_emulators::Source::Manual) {
        return toast(ui, format!("{} ne s'installe pas automatiquement : indique son chemin dans config.toml", profile.name));
    }
    if let Some(exe) = launch::find_executable(data, profile) {
        if !exe.starts_with(launch::managed_dir(data, profile)) {
            return toast(ui, format!("{} est installé hors d'EmuNest : mets-le à jour toi-même", profile.name));
        }
    }
    match run_install(data, ui, profile) {
        Ok(_) => toast(ui, format!("{} est à jour", profile.name)),
        Err(e) => toast(ui, format!("Mise à jour de {} impossible : {e:#}", profile.name)),
    }
}

/// Une ligne de sous-menu a été choisie.
fn handle_sub(data: &Path, ui: &Ui, sub: Sub, game: Option<Game>, cli: &Option<PathBuf>) {
    let need_game = || game.clone();
    let log_err = |r: Result<()>| {
        if let Err(e) = r {
            toast(ui, format!("{e:#}"));
        }
    };
    match sub {
        Sub::ToggleCollection(name) => {
            let Some(g) = need_game() else { return };
            log_err(db(data).and_then(|l| {
                if g.collections.contains(&name) { l.remove_from_collection(&name, &g.path) } else { l.add_to_collection(&name, &g.path) }
            }));
            reload(data, ui);
            toast(ui, if g.collections.contains(&name) { format!("Retiré de « {name} »") } else { format!("Ajouté à « {name} »") });
        }
        Sub::NewCollection => {
            let Some(g) = need_game() else { return };
            if let Some(name) = prompt(ui, "Nouvelle collection", "").filter(|n| !n.is_empty()) {
                log_err(db(data).and_then(|l| l.add_to_collection(&name, &g.path)));
                reload(data, ui);
                toast(ui, format!("Ajouté à « {name} »"));
            }
        }
        Sub::DeleteCollectionList => {
            let names = db(data).and_then(|l| l.collections()).unwrap_or_default();
            show_sub(ui, "Supprimer une collection".into(), names.into_iter().map(|n| (Sub::DeleteCollection(n.clone()), format!("Supprimer « {n} »"))).collect());
        }
        Sub::DeleteCollection(name) => {
            log_err(db(data).and_then(|l| l.delete_collection(&name)));
            reload(data, ui);
            toast(ui, format!("Collection « {name} » supprimée"));
        }
        Sub::PickEmulator => {
            let Some(g) = need_game() else { return };
            let ext = g.path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
            let mut entries: Vec<(Sub, String)> = emunest_emulators::builtin_profiles()
                .unwrap_or_default()
                .into_iter()
                .filter(|p| p.extensions.contains(&ext))
                .map(|p| {
                    let label = if p.name == g.emulator { format!("{}  (actuel)", p.name) } else { p.name.clone() };
                    (Sub::SetEmulator(Some(p.name)), label)
                })
                .collect();
            entries.push((Sub::SetEmulator(None), "Par défaut (celui de la console)".into()));
            show_sub(ui, "Émulateur de ce jeu".into(), entries);
        }
        Sub::SetEmulator(choice) => {
            let Some(g) = need_game() else { return };
            log_err(db(data).and_then(|l| l.set_emulator(&g.path, choice.as_deref())));
            reload(data, ui);
            toast(ui, match choice {
                Some(e) => format!("{} utilisera {e}", g.title),
                None => format!("{} : émulateur par défaut", g.title),
            });
        }
        Sub::EditArgs => {
            let Some(g) = need_game() else { return };
            if let Some(args) = prompt(ui, "Arguments supplémentaires", &g.extra_args) {
                log_err(db(data).and_then(|l| l.set_extra_args(&g.path, &args)));
                reload(data, ui);
            }
        }
        Sub::ResetGame => {
            let Some(g) = need_game() else { return };
            log_err(db(data).and_then(|l| {
                l.set_emulator(&g.path, None)?;
                l.set_extra_args(&g.path, "")
            }));
            reload(data, ui);
            toast(ui, "Réglages du jeu réinitialisés");
        }
        Sub::ChooseCover => {
            let Some(g) = need_game() else { return };
            let picked = rfd::FileDialog::new().set_title("Choisir une jaquette").add_filter("Images", &["png", "jpg", "jpeg"]).pick_file();
            let Some(src) = picked else { return };
            match emunest_metadata::set_custom_cover(data, &g, &src) {
                Ok(()) => {
                    let d = data.to_path_buf();
                    let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::refresh_cover(&w, &g, &d));
                    toast(ui, "Jaquette remplacée");
                }
                Err(e) => toast(ui, format!("{e:#}")),
            }
        }
        Sub::RefetchCover => {
            let Some(g) = need_game() else { return };
            emunest_metadata::reset_cover(data, &g);
            toast(ui, "Nouvelle recherche de la jaquette…");
            fetch_covers(data, ui, std::slice::from_ref(&g));
            let d = data.to_path_buf();
            let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::refresh_cover(&w, &g, &d));
        }
        Sub::RomFolders => show_rom_folders(data, ui),
        Sub::AddRomFolder => {
            let Some(dir) = rfd::FileDialog::new().set_title("Dossier de ROMs").pick_folder() else { return };
            let mut cfg = config::load(data);
            if !cfg.roms.contains(&dir) {
                cfg.roms.push(dir.clone());
            }
            log_err(config::save(data, &cfg));
            toast(ui, format!("Dossier ajouté : {}", dir.display()));
            watch_roms(data.to_path_buf(), ui.clone(), dir, cli.clone());
            let _ = rescan(data, ui, &cfg.roms);
        }
        Sub::RemoveRomFolder(dir) => {
            let mut cfg = config::load(data);
            cfg.roms.retain(|d| d != &dir);
            log_err(config::save(data, &cfg));
            log_err(db(data).and_then(|l| l.remove_under(&dir)));
            reload(data, ui);
            toast(ui, format!("Dossier retiré : {}", dir.display()));
        }
        Sub::Emulators => show_emulators(data, ui),
        Sub::InstallEmulator(name) => {
            if let Some(p) = profile_of(&name) {
                update_emulator(data, ui, &p);
            }
        }
        Sub::OpenData => open_with_default(data),
        Sub::OpenConfig => {
            let path = data.join("config.toml");
            if !path.exists() {
                let _ = config::save(data, &Config::default());
            }
            open_with_default(&path);
        }
    }
}

/// Actions de menu : tourne hors du thread UI.
fn menu_action(data: &Path, ui: &Ui, action: MenuAction, game: Option<Game>, cli: &Option<PathBuf>) {
    match (action, game) {
        (MenuAction::Item(i), game) => {
            let sub = SUB.lock().unwrap().get(i as usize).cloned();
            if let Some(sub) = sub {
                handle_sub(data, ui, sub, game, cli);
            }
        }
        (MenuAction::AppSettings, _) => show_settings(data, ui),
        (MenuAction::Collections, Some(g)) => show_collections(data, ui, &g),
        (MenuAction::GameSettings, Some(g)) => show_game_settings(ui, &g),
        (MenuAction::Cover, Some(_)) => show_sub(
            ui,
            "Jaquette".into(),
            vec![(Sub::ChooseCover, "Choisir une image…".into()), (Sub::RefetchCover, "Rechercher à nouveau automatiquement".into())],
        ),
        (MenuAction::Notes, Some(g)) => {
            if let Some(note) = prompt(ui, &format!("Notes · {}", g.title), &g.note) {
                if let Err(e) = db(data).and_then(|l| l.set_note(&g.path, &note)) {
                    toast(ui, format!("{e:#}"));
                }
                reload(data, ui);
            }
        }
        (MenuAction::OpenEmulator, Some(g)) => {
            let Some(profile) = profile_of(&g.emulator) else { return toast(ui, "Émulateur inconnu") };
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
        (MenuAction::UpdateEmulator, Some(g)) => match profile_of(&g.emulator) {
            Some(p) => update_emulator(data, ui, &p),
            None => toast(ui, "Émulateur inconnu"),
        },
        (MenuAction::OpenFolder, Some(g)) => explorer_select(&g.path),
        _ => {} // Jouer / favori : gérés par l'interface
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
    type Job = Box<dyn FnOnce(&Path, &Ui) + Send>;
    let run_busy: Arc<dyn Fn(Job)> = {
        let (data, weak, busy) = (data.clone(), weak.clone(), busy.clone());
        Arc::new(move |job: Job| {
            if busy.swap(true, Ordering::SeqCst) {
                return;
            }
            let (data, weak, busy) = (data.clone(), weak.clone(), busy.clone());
            std::thread::spawn(move || {
                job(&data, &weak);
                busy.store(false, Ordering::SeqCst);
            });
        })
    };

    let rb = run_busy.clone();
    emunest_ui::on_launch(&win, move |game| rb(Box::new(move |d, ui| play(d, ui, game))));
    let (rb, cli) = (run_busy.clone(), roms.clone());
    emunest_ui::on_menu(move |action, game| {
        let cli = cli.clone();
        rb(Box::new(move |d, ui| menu_action(d, ui, action, game, &cli)))
    });
    let rb = run_busy.clone();
    emunest_ui::on_settings(move || rb(Box::new(|d, ui| show_settings(d, ui))));
    {
        let (data, weak) = (data.clone(), weak.clone());
        emunest_ui::on_favorite(move |game| {
            let (data, weak) = (data.clone(), weak.clone());
            std::thread::spawn(move || {
                if let Ok(lib) = db(&data) {
                    let _ = lib.set_favorite(&game.path, !game.favorite);
                }
                reload(&data, &weak);
            });
        });
    }

    // Tout le travail lent (scan, réseau) hors du thread UI : la fenêtre s'affiche tout de suite.
    std::thread::spawn(move || {
        reload(&data, &weak); // la bibliothèque déjà connue s'affiche sans attendre le scan
        let dirs = rom_dirs(&data, &roms);
        if dirs.is_empty() {
            if let Ok(games) = db(&data).and_then(|l| l.games()) {
                fetch_covers(&data, &weak, &games);
            }
            return;
        }
        if let Err(e) = rescan(&data, &weak, &dirs) {
            tracing::error!("scan : {e:#}");
        }
        for dir in dirs {
            watch_roms(data.clone(), weak.clone(), dir, roms.clone());
        }
    });

    win.run()?;
    Ok(())
}
