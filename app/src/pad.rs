//! Manette (gilrs) : D-pad et stick gauche pour naviguer, LB/RB pour sauter de 5, A lance, B ferme le message.
use emunest_ui::{MainWindow, Pad, PadKind, Weak};
use gilrs::{Axis, Button, EventType, Gamepad, Gilrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Vrai pendant un jeu dont le curseur se pilote à la souris (ex. Mario Galaxy, pointeur de l'écran) :
/// le stick droit déplace alors le curseur Windows et R3 clique.
pub static POINTER: AtomicBool = AtomicBool::new(false);
const POINTER_SPEED: f32 = 14.0; // pixels par tour de boucle (30 ms) à pleine inclinaison

fn move_cursor(dx: f32, dy: f32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: appels Win32 simples sur une structure locale
    unsafe {
        if GetCursorPos(&mut p) != 0 {
            SetCursorPos(p.x + dx as i32, p.y + dy as i32);
        }
    }
}

fn click(down: bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{mouse_event, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP};
    // SAFETY: simple envoi d'un évènement souris
    unsafe { mouse_event(if down { MOUSEEVENTF_LEFTDOWN } else { MOUSEEVENTF_LEFTUP }, 0, 0, 0, 0) };
}

const FIRST_REPEAT: Duration = Duration::from_millis(380);
const REPEAT: Duration = Duration::from_millis(130);
const STICK_ON: f32 = 0.55;
const STICK_RELEASE: f32 = 0.3;

/// Famille de la manette d'après son fabricant (USB) ou son nom.
fn kind_of(g: &Gamepad) -> PadKind {
    let name = g.name().to_lowercase();
    match g.vendor_id() {
        Some(0x054c) => return PadKind::PlayStation,
        Some(0x045e) => return PadKind::Xbox,
        Some(0x057e) => return PadKind::Nintendo,
        _ => {}
    }
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if has(&["playstation", "dualshock", "dualsense", "ps4", "ps5", "ps3", "wireless controller"]) {
        PadKind::PlayStation
    } else if has(&["xbox", "xinput", "microsoft"]) {
        PadKind::Xbox
    } else if has(&["nintendo", "switch", "joy-con", "pro controller"]) {
        PadKind::Nintendo
    } else {
        PadKind::Generic
    }
}

fn map_button(b: Button, kind: PadKind) -> Option<Pad> {
    // Nintendo : le bouton de droite (A) confirme, celui du bas (B) revient ; ailleurs c'est l'inverse
    let (accept, back) = if kind == PadKind::Nintendo { (Button::East, Button::South) } else { (Button::South, Button::East) };
    Some(match b {
        Button::DPadLeft => Pad::Step(-1),
        Button::DPadRight => Pad::Step(1),
        Button::DPadUp => Pad::Up,
        Button::DPadDown => Pad::Down,
        Button::LeftTrigger => Pad::Filter(-1),
        Button::RightTrigger => Pad::Filter(1),
        Button::North => Pad::Favorite,
        Button::Select => Pad::Options,
        Button::Start => Pad::Start,
        Button::West => Pad::Extra,
        x if x == accept => Pad::Accept,
        x if x == back => Pad::Back,
        _ => return None,
    })
}

/// Ce qui est maintenu : sert à répéter le mouvement tant que la direction est tenue.
#[derive(Clone, Copy, PartialEq)]
enum Source {
    Button(Button),
    Stick,
}

/// Écoute les manettes dans un thread à part. Rien n'est envoyé pendant qu'un jeu tourne (`busy`).
pub fn spawn(ui: Weak<MainWindow>, busy: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut gilrs = match Gilrs::new() {
            Ok(g) => g,
            Err(e) => return tracing::warn!("manette indisponible : {e}"),
        };
        // Steam Input (et consorts) montre souvent la même manette deux fois : on ignore le doublon quasi simultané
        let last = std::cell::Cell::new(None::<(Pad, Instant)>);
        let send = |p: Pad| {
            let now = Instant::now();
            if matches!(last.get(), Some((lp, t)) if lp == p && now.duration_since(t) < Duration::from_millis(40)) {
                return;
            }
            last.set(Some((p, now)));
            if !busy.load(Ordering::SeqCst) {
                let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::pad_input(&w, p));
            }
        };
        let mut held: Option<(Source, Pad, Instant)> = None;
        let mut kind = PadKind::Generic;
        let (mut rx, mut ry) = (0.0f32, 0.0f32); // stick droit (pour le curseur)
        loop {
            if let Some(ev) = gilrs.next_event_blocking(Some(Duration::from_millis(30))) {
                // symboles de la manette qui vient de parler (changent si on en branche une autre)
                let seen = kind_of(&gilrs.gamepad(ev.id));
                if seen != kind {
                    kind = seen;
                    let _ = ui.upgrade_in_event_loop(move |w| emunest_ui::set_pad_kind(&w, seen));
                }
                match ev.event {
                    EventType::ButtonPressed(Button::RightThumb, _) if POINTER.load(Ordering::SeqCst) => click(true),
                    EventType::ButtonReleased(Button::RightThumb, _) if POINTER.load(Ordering::SeqCst) => click(false),
                    EventType::AxisChanged(Axis::RightStickX, v, _) => rx = v,
                    EventType::AxisChanged(Axis::RightStickY, v, _) => ry = v,
                    EventType::ButtonPressed(b, _) => {
                        if let Some(p) = map_button(b, kind) {
                            send(p);
                            // seuls les déplacements se répètent (pas A, pas B)
                            if matches!(p, Pad::Step(_) | Pad::Up | Pad::Down) {
                                held = Some((Source::Button(b), p, Instant::now() + FIRST_REPEAT));
                            }
                        }
                    }
                    EventType::ButtonReleased(b, _) => {
                        if matches!(held, Some((Source::Button(h), _, _)) if h == b) {
                            held = None;
                        }
                    }
                    EventType::AxisChanged(axis @ (Axis::LeftStickX | Axis::LeftStickY), v, _) => {
                        // X : gauche / droite ; Y (positif = haut) : haut / bas
                        let x_axis = axis == Axis::LeftStickX;
                        // hystérésis : une fois poussé, le stick reste « tenu » jusqu'à 0.3 (évite les doubles pas)
                        let holding = matches!(held, Some((Source::Stick, cur, _)) if matches!(cur, Pad::Step(_)) == x_axis);
                        let on = if holding { STICK_RELEASE } else { STICK_ON };
                        let dir = match axis {
                            Axis::LeftStickX if v > on => Some(Pad::Step(1)),
                            Axis::LeftStickX if v < -on => Some(Pad::Step(-1)),
                            Axis::LeftStickY if v > on => Some(Pad::Up),
                            Axis::LeftStickY if v < -on => Some(Pad::Down),
                            _ => None,
                        };
                        match (dir, held) {
                            (Some(d), Some((Source::Stick, cur, _))) if d == cur => {}
                            (Some(d), _) => {
                                send(d);
                                held = Some((Source::Stick, d, Instant::now() + FIRST_REPEAT));
                            }
                            // seul l'axe qui tient la direction peut la relâcher (l'autre axe qui revient à zéro ne compte pas)
                            (None, Some((Source::Stick, _, _))) if holding => held = None,
                            _ => {}
                        }
                    }
                    EventType::Disconnected => held = None,
                    _ => {}
                }
            }
            if POINTER.load(Ordering::SeqCst) && (rx.abs() > 0.12 || ry.abs() > 0.12) {
                move_cursor(rx * POINTER_SPEED, -ry * POINTER_SPEED); // Y de la manette : positif = haut
            }
            if let Some((src, p, next)) = held {
                if Instant::now() >= next {
                    send(p);
                    held = Some((src, p, Instant::now() + REPEAT));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_map_like_big_picture() {
        use PadKind::*;
        assert_eq!(map_button(Button::South, Xbox), Some(Pad::Accept));
        assert_eq!(map_button(Button::East, PlayStation), Some(Pad::Back));
        // Nintendo : confirmer = bouton de droite
        assert_eq!(map_button(Button::East, Nintendo), Some(Pad::Accept));
        assert_eq!(map_button(Button::South, Nintendo), Some(Pad::Back));
        assert_eq!(map_button(Button::DPadRight, Generic), Some(Pad::Step(1)));
        assert_eq!(map_button(Button::DPadUp, Generic), Some(Pad::Up));
        assert_eq!(map_button(Button::Start, Generic), Some(Pad::Start));
        assert_eq!(map_button(Button::LeftTrigger, Xbox), Some(Pad::Filter(-1)));
        assert_eq!(map_button(Button::North, Xbox), Some(Pad::Favorite));
        assert_eq!(map_button(Button::Select, Xbox), Some(Pad::Options));
        assert_eq!(map_button(Button::C, Xbox), None);
    }
}
