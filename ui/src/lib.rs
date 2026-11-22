//! Interface Slint : fenêtre principale + glue vers les modèles du core.
use emunest_core::Game;
use emunest_emulators::platform_name;
use slint::{Color, Image, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;

slint::include_modules!();

pub use slint::{ComponentHandle, Weak};

/// Filtre de la liste : tout, favoris, récents ou une console.
#[derive(Clone, Debug, PartialEq, Default)]
enum Filter {
    #[default]
    All,
    Favorites,
    Recent,
    Platform(String),
}

impl Filter {
    fn label(&self) -> String {
        match self {
            Filter::All => "Tout".into(),
            Filter::Favorites => "Favoris".into(),
            Filter::Recent => "Récents".into(),
            Filter::Platform(p) => platform_name(p).into(),
        }
    }
}

/// Actions du menu Options d'un jeu ; les actions qui touchent au système sont exécutées par l'application.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MenuAction {
    Play,
    ToggleFavorite,
    OpenEmulator,
    UpdateEmulator,
    OpenFolder,
    ChooseCover,
    RefetchCover,
}

/// Images déjà décodées d'un jeu (évite de relire les fichiers à chaque filtre ou frappe).
#[derive(Clone)]
struct Art {
    cover: Option<Image>,
    bg: Option<Image>,
    palette: Option<[(u8, u8, u8); 3]>,
}

#[derive(Default)]
struct View {
    all: Vec<Game>,
    /// jeux affichés dans l'ordre du carrousel (après filtre et recherche)
    visible: Vec<Game>,
    cache: PathBuf,
    filter: Filter,
    query: String,
    searching: bool,
    menu: Vec<MenuAction>,
    art: HashMap<PathBuf, Art>,
    on_favorite: Option<Rc<dyn Fn(Game)>>,
    on_menu: Option<Rc<dyn Fn(MenuAction, Game)>>,
}

thread_local! {
    // état du thread UI
    static VIEW: RefCell<View> = RefCell::new(View::default());
    static TOAST: Timer = Timer::default();
    // réponse en attente à la question affichée
    static ANSWER: RefCell<Option<Sender<Answer>>> = RefCell::new(None);
}

/// Appelle `f` avec le jeu choisi (Entrée, bouton Jouer ou clic sur la jaquette sélectionnée).
pub fn on_launch(win: &MainWindow, f: impl Fn(Game) + 'static) {
    win.on_launch(move |i| {
        if let Some(g) = VIEW.with_borrow(|v| v.visible.get(i as usize).cloned()) {
            f(g);
        }
    });
}

/// Appelle `f` quand l'utilisateur veut (dé)marquer un jeu comme favori.
pub fn on_favorite(f: impl Fn(Game) + 'static) {
    VIEW.with_borrow_mut(|v| v.on_favorite = Some(Rc::new(f)));
}

/// Appelle `f` pour les actions du menu Options qui touchent au système (hors lancer / favori).
pub fn on_menu(f: impl Fn(MenuAction, Game) + 'static) {
    VIEW.with_borrow_mut(|v| v.on_menu = Some(Rc::new(f)));
}

fn current_game(win: &MainWindow) -> Option<Game> {
    VIEW.with_borrow(|v| v.visible.get(win.get_current() as usize).cloned())
}

/// Réponse à une question de la fenêtre modale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Answer {
    Yes,
    No,
    /// action secondaire (ex. « Ouvrir le dossier »)
    Extra,
}

/// Famille de manette : décide des symboles affichés dans l'aide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadKind {
    Xbox,
    PlayStation,
    Nintendo,
    Generic,
}

/// Règle la famille de manette : l'interface dessine alors les vrais boutons (A/B, ×/○…). Thread UI.
pub fn set_pad_kind(win: &MainWindow, kind: PadKind) {
    win.set_pad_kind(match kind {
        PadKind::Xbox | PadKind::Generic => 0,
        PadKind::PlayStation => 1,
        PadKind::Nintendo => 2,
    });
}

/// Ce que la manette demande à l'interface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pad {
    /// déplace la sélection de n jeux
    Step(i32),
    /// A : lance le jeu sélectionné
    Accept,
    /// B : ferme le message affiché
    Back,
    /// X : action secondaire de la fenêtre modale
    Extra,
    /// LB / RB : change de filtre (console, favoris, récents)
    Filter(i32),
    /// Y : favori
    Favorite,
    /// Select : menu Options
    Options,
}

/// Applique une action de manette. Thread UI.
pub fn pad_input(win: &MainWindow, input: Pad) {
    win.set_pad_active(true);
    let n = win.get_games().row_count() as i32;
    match win.get_dialog_mode() {
        0 => {}
        4 => {
            // menu Options : croix = choisir, A = valider, B = fermer
            let len = win.get_menu_items().row_count() as i32;
            match input {
                Pad::Step(d) if d.abs() == 1 => win.set_menu_index((win.get_menu_index() + d).clamp(0, (len - 1).max(0))),
                Pad::Accept => win.invoke_menu_activate(win.get_menu_index()),
                Pad::Back => win.invoke_dialog_cancel(),
                _ => {}
            }
            return;
        }
        mode => {
            match input {
                Pad::Accept if matches!(mode, 1 | 3) => win.invoke_dialog_accept(),
                Pad::Back if matches!(mode, 1 | 3) => win.invoke_dialog_cancel(),
                Pad::Extra if mode == 3 => win.invoke_dialog_extra(),
                _ => {}
            }
            return;
        }
    }
    match input {
        Pad::Step(d) if n > 0 => win.set_selected((win.get_current() + d).clamp(0, n - 1)),
        Pad::Accept if n > 0 => win.invoke_launch(win.get_current()),
        Pad::Back => win.set_toast("".into()),
        Pad::Filter(d) => step_filter(win, d),
        Pad::Favorite => toggle_favorite(win),
        Pad::Options => open_menu(win),
        _ => {}
    }
}

/// Pose une question ; la réponse part sur `tx`. Thread UI.
/// `missing_file` = vrai pour l'écran « fichier manquant » (Vérifier / Ouvrir le dossier / Annuler),
/// faux pour la proposition d'installation (Installer / Annuler).
pub fn ask(win: &MainWindow, title: &str, text: &str, missing_file: bool, tx: Sender<Answer>) {
    ANSWER.with_borrow_mut(|a| *a = Some(tx));
    win.set_dialog_title(title.into());
    win.set_dialog_text(text.into());
    win.set_dialog_mode(if missing_file { 3 } else { 1 });
}

/// Affiche une barre de progression (`fraction` entre 0 et 1). Thread UI.
pub fn show_progress(win: &MainWindow, title: &str, text: &str, fraction: f32) {
    win.set_dialog_title(title.into());
    win.set_dialog_text(text.into());
    win.set_progress(fraction.clamp(0.0, 1.0));
    win.set_dialog_mode(2);
}

pub fn close_dialog(win: &MainWindow) {
    win.set_dialog_mode(0);
}

/// Message passager en bas de l'écran (6 s).
pub fn toast(win: &MainWindow, msg: &str) {
    win.set_toast(msg.into());
    let weak = win.as_weak();
    TOAST.with(|t| {
        t.start(TimerMode::SingleShot, Duration::from_secs(6), move || {
            if let Some(w) = weak.upgrade() {
                w.set_toast("".into());
            }
        })
    });
}

fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::from_rgb_u8(r, g, b)
}

fn hsl(hue: u32, s: f32, l: f32) -> Color {
    let hue = hue % 360;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((hue as f32 / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match hue / 60 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let u = |v: f32| ((v + m) * 255.0) as u8;
    Color::from_rgb_u8(u(r), u(g), u(b))
}

/// Deux couleurs stables par plateforme (teinte dérivée de l'id) pour le dégradé de pochette.
fn platform_colors(id: &str) -> (Color, Color) {
    let hue = id.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) % 360;
    (hsl(hue, 0.60, 0.45), hsl(hue + 50, 0.65, 0.20))
}

/// Durée de jeu lisible : « moins d'1 min », « 25 min », « 3 h 07 ».
fn duration_text(secs: i64) -> String {
    match secs {
        s if s < 60 => "moins d'1 min".into(),
        s if s < 3600 => format!("{} min", s / 60),
        s => format!("{} h {:02}", s / 3600, (s % 3600) / 60),
    }
}

/// « Jamais lancé » ou « 3 h 07 jouées · hier ».
fn stats_text(g: &Game) -> String {
    let fav = if g.favorite { "Favori  ·  " } else { "" };
    if g.last_played == 0 {
        return format!("{fav}Jamais lancé");
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let when = match (now - g.last_played).max(0) / 86_400 {
        0 => "aujourd'hui".to_string(),
        1 => "hier".to_string(),
        d => format!("il y a {d} jours"),
    };
    format!("{fav}{} jouées  ·  {when}", duration_text(g.play_secs))
}

fn item(g: &Game, v: &mut View) -> GameItem {
    let (color, color2) = platform_colors(&g.platform);
    let cache = v.cache.clone();
    let art = v
        .art
        .entry(g.path.clone())
        .or_insert_with(|| Art {
            cover: emunest_metadata::cover_path(&cache, g).and_then(|p| Image::load_from_path(&p).ok()),
            bg: emunest_metadata::bg_path(&cache, g).and_then(|p| Image::load_from_path(&p).ok()),
            palette: emunest_metadata::palette(&cache, g),
        })
        .clone();
    let pal = art.palette;
    GameItem {
        bg: art.bg.unwrap_or_default(),
        p1: pal.map_or(color, |p| rgb(p[0])),
        p2: pal.map_or(color2, |p| rgb(p[1])),
        p3: pal.map_or(color, |p| rgb(p[2])),
        title: g.title.as_str().into(),
        platform: platform_name(&g.platform).into(),
        emulator: g.emulator.as_str().into(),
        initial: g.title.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into(),
        path: g.path.to_string_lossy().as_ref().into(),
        seed: g.title.bytes().fold(17u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32)) as i32 & 0x7fff_ffff,
        // forme de la tuile : celle de la vraie jaquette, sinon celle du boîtier de la console
        aspect: art.cover.as_ref().map_or_else(
            || emunest_emulators::cover_aspect(&g.platform),
            |c| c.size().width as f32 / c.size().height.max(1) as f32,
        ),
        favorite: g.favorite,
        stats: stats_text(g).into(),
        has_cover: art.cover.is_some(),
        cover: art.cover.unwrap_or_default(),
        color,
        color2,
    }
}

/// Filtres proposés d'après la bibliothèque : Tout, Favoris et Récents s'ils servent, puis les consoles présentes.
fn filters_of(all: &[Game]) -> Vec<Filter> {
    let mut f = vec![Filter::All];
    if all.iter().any(|g| g.favorite) {
        f.push(Filter::Favorites);
    }
    if all.iter().any(|g| g.last_played > 0) {
        f.push(Filter::Recent);
    }
    let mut platforms: Vec<&str> = all.iter().map(|g| g.platform.as_str()).collect();
    platforms.sort_by_key(|p| platform_name(p).to_string());
    platforms.dedup();
    f.extend(platforms.into_iter().map(|p| Filter::Platform(p.to_string())));
    f
}

/// Recalcule la liste affichée (filtre + recherche) et met l'interface à jour.
/// `keep_selection` : garde le même jeu sélectionné s'il est toujours là, sinon revient au premier.
fn apply(win: &MainWindow, keep_selection: bool) {
    let prev = if keep_selection { current_game(win).map(|g| g.path) } else { None };
    let (items, labels, filter_idx, query, searching) = VIEW.with_borrow_mut(|v| {
        let filters = filters_of(&v.all);
        if !filters.contains(&v.filter) {
            v.filter = Filter::All;
        }
        let q = v.query.to_lowercase();
        let mut vis: Vec<Game> = v
            .all
            .iter()
            .filter(|g| match &v.filter {
                Filter::All => true,
                Filter::Favorites => g.favorite,
                Filter::Recent => g.last_played > 0,
                Filter::Platform(p) => &g.platform == p,
            })
            .filter(|g| q.is_empty() || g.title.to_lowercase().contains(&q))
            .cloned()
            .collect();
        if v.filter == Filter::Recent {
            vis.sort_by(|a, b| b.last_played.cmp(&a.last_played));
            vis.truncate(24);
        }
        v.visible = vis;
        let visible = v.visible.clone();
        let items: Vec<GameItem> = visible.iter().map(|g| item(g, v)).collect();
        let labels: Vec<SharedString> = filters.iter().map(|f| f.label().into()).collect();
        let idx = filters.iter().position(|f| *f == v.filter).unwrap_or(0);
        (items, labels, idx, v.query.clone(), v.searching)
    });
    win.set_games(ModelRc::new(VecModel::from(items)));
    win.set_filters(ModelRc::new(VecModel::from(labels)));
    win.set_filter_index(filter_idx as i32);
    win.set_search(query.into());
    win.set_searching(searching);
    let idx = prev
        .and_then(|p| VIEW.with_borrow(|v| v.visible.iter().position(|g| g.path == p)))
        .unwrap_or(0);
    win.set_selected(idx as i32);
}

fn select_filter(win: &MainWindow, index: usize) {
    VIEW.with_borrow_mut(|v| {
        if let Some(f) = filters_of(&v.all).into_iter().nth(index) {
            v.filter = f;
        }
    });
    apply(win, false);
}

fn step_filter(win: &MainWindow, d: i32) {
    let (n, cur) = VIEW.with_borrow(|v| {
        let f = filters_of(&v.all);
        (f.len() as i32, f.iter().position(|x| *x == v.filter).unwrap_or(0) as i32)
    });
    select_filter(win, (cur + d).rem_euclid(n.max(1)) as usize);
}

fn toggle_favorite(win: &MainWindow) {
    let cb = VIEW.with_borrow(|v| v.on_favorite.clone());
    if let (Some(cb), Some(g)) = (cb, current_game(win)) {
        cb(g);
    }
}

/// Ouvre le menu Options du jeu sélectionné.
fn open_menu(win: &MainWindow) {
    let Some(g) = current_game(win) else { return };
    let entries = [
        (MenuAction::Play, "Jouer".to_string()),
        (MenuAction::ToggleFavorite, if g.favorite { "Retirer des favoris" } else { "Ajouter aux favoris" }.to_string()),
        (MenuAction::OpenEmulator, format!("Ouvrir {} (réglages)", g.emulator)),
        (MenuAction::UpdateEmulator, format!("Mettre à jour {}", g.emulator)),
        (MenuAction::OpenFolder, "Ouvrir le dossier du jeu".to_string()),
        (MenuAction::ChooseCover, "Choisir une jaquette…".to_string()),
        (MenuAction::RefetchCover, "Rechercher la jaquette à nouveau".to_string()),
    ];
    VIEW.with_borrow_mut(|v| v.menu = entries.iter().map(|(a, _)| *a).collect());
    let labels: Vec<SharedString> = entries.iter().map(|(_, l)| l.as_str().into()).collect();
    win.set_menu_items(ModelRc::new(VecModel::from(labels)));
    win.set_menu_index(0);
    win.set_dialog_title(g.title.as_str().into());
    win.set_dialog_text("".into());
    win.set_dialog_mode(4);
}

/// Touches qui ne sont pas de la navigation : recherche (« / » puis on tape), Tab = console suivante,
/// O = options, F = favori, Échap = efface recherche / filtre avant de quitter. Renvoie vrai si la touche est consommée.
fn key_action(win: &MainWindow, text: &str) -> bool {
    if let Some(n) = text.strip_prefix('#').and_then(|n| n.parse::<usize>().ok()) {
        select_filter(win, n); // clic sur une pastille de filtre
        return true;
    }
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else { return false };
    let typed = !c.is_control() && !('\u{e000}'..='\u{f8ff}').contains(&c); // exclut les touches de navigation de Slint
    let searching = VIEW.with_borrow(|v| v.searching);
    if searching {
        VIEW.with_borrow_mut(|v| match c {
            '\u{1b}' => {
                v.query.clear();
                v.searching = false;
            }
            '\n' | '\r' => v.searching = false,
            '\u{8}' => {
                v.query.pop();
            }
            c if typed => v.query.push(c),
            _ => {}
        });
        if matches!(c, '\u{1b}' | '\n' | '\r' | '\u{8}') || typed {
            apply(win, false);
            return true;
        }
        return false; // flèches : on peut naviguer dans les résultats pendant la frappe
    }
    match c {
        '/' => {
            VIEW.with_borrow_mut(|v| v.searching = true);
            apply(win, true);
        }
        '\t' => step_filter(win, 1),
        '\u{19}' => step_filter(win, -1),
        'o' | 'O' => open_menu(win),
        'f' | 'F' => toggle_favorite(win),
        '\u{1b}' => {
            let active = VIEW.with_borrow(|v| !v.query.is_empty() || v.filter != Filter::All);
            if !active {
                return false;
            }
            VIEW.with_borrow_mut(|v| {
                v.query.clear();
                v.filter = Filter::All;
            });
            apply(win, false);
        }
        _ => return false,
    }
    true
}

/// À appeler depuis le thread UI : remplace la bibliothèque affichée (garde filtre, recherche et sélection).
pub fn set_games(win: &MainWindow, games: &[Game], cache: &Path) {
    VIEW.with_borrow_mut(|v| {
        v.all = games.to_vec();
        v.cache = cache.to_path_buf();
    });
    apply(win, true);
}

/// Recharge la jaquette d'un seul jeu (après téléchargement ou choix manuel). Thread UI.
pub fn refresh_cover(win: &MainWindow, game: &Game, cache: &Path) {
    let _ = cache;
    let (i, it) = VIEW.with_borrow_mut(|v| {
        v.art.remove(&game.path);
        let i = v.visible.iter().position(|g| g.path == game.path)?;
        let g = v.visible[i].clone();
        Some((i, item(&g, v)))
    })
    .map_or((None, None), |(i, it)| (Some(i), Some(it)));
    if let (Some(i), Some(it)) = (i, it) {
        win.get_games().set_row_data(i, it);
    }
}

/// Lance le déplacement (HTCAPTION = 2) ou le redimensionnement (HTLEFT = 10 … HTBOTTOMRIGHT = 17) natif
/// de Windows : la fenêtre n'a pas de cadre, on réutilise la mécanique du système depuis nos poignées.
#[cfg(windows)]
fn native_drag(win: &slint::Window, hit: usize) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_NCLBUTTONDOWN};
    if let Ok(h) = win.window_handle().window_handle() {
        if let RawWindowHandle::Win32(w) = h.as_raw() {
            // SAFETY: appels Win32 sans pointeur, sur le handle valide de notre propre fenêtre
            unsafe {
                ReleaseCapture();
                PostMessageW(w.hwnd.get() as _, WM_NCLBUTTONDOWN, hit, 0);
            }
        }
    }
}

#[cfg(not(windows))]
fn native_drag(_: &slint::Window, _: usize) {}

pub fn create(fullscreen: bool) -> Result<MainWindow, slint::PlatformError> {
    let win = MainWindow::new()?;
    win.window().set_fullscreen(fullscreen);
    win.set_windowed(!fullscreen);
    let weak = win.as_weak();
    win.on_resize_edge(move |code| {
        if let Some(w) = weak.upgrade() {
            native_drag(w.window(), code as usize);
        }
    });
    let weak = win.as_weak();
    win.on_move_window(move || {
        if let Some(w) = weak.upgrade() {
            native_drag(w.window(), 2);
        }
    });
    let weak = win.as_weak();
    win.on_minimize(move || {
        if let Some(w) = weak.upgrade() {
            w.window().set_minimized(true);
        }
    });
    let weak = win.as_weak();
    win.on_toggle_fullscreen(move || {
        if let Some(w) = weak.upgrade() {
            let win = w.window();
            if win.is_fullscreen() {
                // retour en fenêtre : grande (88 % de l'écran) et centrée, pas minuscule
                let screen = win.size();
                win.set_fullscreen(false);
                win.set_size(slint::PhysicalSize::new(screen.width * 88 / 100, screen.height * 88 / 100));
                win.set_position(slint::PhysicalPosition::new((screen.width * 6 / 100) as i32, (screen.height * 6 / 100) as i32));
                w.set_windowed(true);
            } else {
                win.set_fullscreen(true);
                w.set_windowed(false);
            }
        }
    });
    let weak = win.as_weak();
    win.on_key_action(move |text| weak.upgrade().is_some_and(|w| key_action(&w, text.as_str())));
    let weak = win.as_weak();
    win.on_menu_activate(move |i| {
        let Some(w) = weak.upgrade() else { return };
        w.set_dialog_mode(0);
        let action = VIEW.with_borrow(|v| v.menu.get(i as usize).copied());
        let (Some(action), Some(game)) = (action, current_game(&w)) else { return };
        match action {
            MenuAction::Play => w.invoke_launch(w.get_current()),
            MenuAction::ToggleFavorite => toggle_favorite(&w),
            other => {
                if let Some(cb) = VIEW.with_borrow(|v| v.on_menu.clone()) {
                    cb(other, game);
                }
            }
        }
    });
    win.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    Ok(win)
}
