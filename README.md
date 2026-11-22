# EmuNest

Launcher d'émulateurs en **Rust pur** (pas de webview), rapide et léger, avec une interface façon Steam Big Picture
(verre liquide, jaquettes, fond animé) utilisable **à la manette ou au clavier**. Windows d'abord.

## Fonctionnalités

- **Bibliothèque** : scanne tes dossiers de ROMs (sous-dossiers `ps2`, `psp`, `switch`… pour départager les `.iso`),
  stockée en SQLite, avec **rescan automatique** quand tu ajoutes ou retires un jeu.
- **Jaquettes automatiques** (libretro-thumbnails, puis Wikipédia en repli), à la vraie forme du boîtier de chaque
  console ; jaquette personnalisable depuis le menu Options.
- **Carrousel en boucle** : la jaquette sélectionnée reste au centre et les jeux défilent autour d'elle (dès 7 jeux) ;
  avec moins de jeux, la rangée est simplement centrée.
- **Lancement** avec l'émulateur de la console ; la fenêtre se réduit pendant la partie et revient à la fermeture.
  Le **temps de jeu** est enregistré.
- **Installation automatique** des émulateurs (dernière release officielle, SHA-256 vérifié quand il est publié)
  dans `~/.emunest/emulators/`. Les émulateurs déjà installés se déclarent dans `config.toml`.
- **Favoris**, **récents**, **filtre par console**, **recherche**, **collections**, **notes** et **réglages par jeu**.
- **Fichiers à fournir toi-même** (BIOS, clés) : un écran t'indique où les poser. EmuNest ne télécharge **jamais**
  de BIOS ni de ROM.
- **Manette** détectée (Xbox, PlayStation, Nintendo) avec les vrais symboles des boutons, et clavier à l'écran
  pour saisir du texte.
- **Sauvegardes des saves** (zip datés, restauration), **galerie de captures** par jeu, **mode télé** (`--tv`).
- **Sons et intro** réglables ou coupables (voir [Sons](#sons)).
- **Mise à jour automatique** depuis les releases GitHub (jamais sur un exécutable compilé sur place).

Émulateurs pris en charge : Dolphin (GameCube, Wii), PCSX2 (PS2), DuckStation (PS1), PPSSPP (PSP), mGBA (GB/GBC/GBA),
Ryujinx (Switch, manuel), Cemu (Wii U), RPCS3 (PS3), melonDS (DS), Azahar (3DS). Un profil = un petit fichier TOML dans
`emulators/profiles/`.

## Installation

Télécharge `EmuNest-Setup-X.Y.Z.exe` (installeur, sans droits administrateur) ou le zip portable depuis les
[releases](../../releases). EmuNest se met ensuite à jour tout seul (Réglages › Mises à jour).

## Utilisation

```powershell
emunest.exe [--windowed] [--tv] [dossier_de_roms]
```

Sans argument, EmuNest utilise les dossiers de ROMs de `~/.emunest/config.toml` :

```toml
roms = ['C:\Jeux\Roms']

[emulators]            # facultatif : émulateurs déjà installés ailleurs
PCSX2 = 'C:\Emulateurs\PCSX2\pcsx2-qt.exe'
```

Les données (bibliothèque, jaquettes, émulateurs installés) sont dans `~/.emunest` (variable `EMUNEST_DATA` pour changer).

### Touches

| Action | Clavier | Manette |
|---|---|---|
| Naviguer | ← → | croix / stick gauche |
| Jouer | Entrée | A (✕ sur PlayStation) |
| Changer de console | Tab / Maj+Tab | LB / RB |
| Options du jeu | O | Select |
| Réglages d'EmuNest | S | (menu Options › Réglages) |
| Favori | F | Y (△) |
| Rechercher | `/` puis taper | |
| Quitter / effacer le filtre | Échap | |

## Sons

Par défaut, les sons de l'interface sont **synthétisés** : aucun fichier audio n'est livré avec EmuNest.

Pour un rendu plus naturel, tu peux déposer tes propres échantillons (wav, ogg, flac ou mp3) dans
`~/.emunest/sounds/` ; ils sont joués à la bonne hauteur, quelle que soit leur note d'origine :

| Fichier | Rôle |
|---|---|
| `bell.*` | notes courtes (déplacement, validation…) |
| `chime.*` | notes longues et résonantes |
| `pad.*` | nappe de l'intro et du lancement |
| `bass.*` | basse douce (un 808, par exemple) |

Chaque fichier est facultatif. Vérifie la licence des échantillons que tu utilises : EmuNest ne les copie nulle part.

## Développement

Workspace Cargo : `core` (scan, SQLite, lancement, installation), `emulators` (profils), `metadata` (jaquettes),
`sound`, `ui` (Slint), `app` (binaire), `installer` (Inno Setup).

```powershell
cargo test --workspace
cargo run --release -p emunest
```

`tools/make-icon` régénère l'icône depuis `logo/emunest.svg`. Chaque push sur `master` publie une version
(GitHub Actions).

## Licence

[MIT](LICENSE).