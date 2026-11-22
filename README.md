# EmuNest

Launcher d'émulateurs en **Rust pur** (pas de webview), rapide et léger, avec une interface façon Steam Big Picture
(verre liquide, jaquettes, fond animé) utilisable **à la manette ou au clavier**. Windows d'abord.

## Ce qu'il fait

- **Bibliothèque** : scanne ton dossier de ROMs (sous-dossiers `ps2`, `psp`, `switch`… pour départager les `.iso`),
  stockée en SQLite ; **rescan automatique** quand tu ajoutes ou retires un jeu.
- **Jaquettes automatiques** (libretro-thumbnails, puis Wikipédia en repli), à la vraie forme du boîtier de chaque console ;
  jaquette personnalisable depuis le menu Options.
- **Lancement** avec l'émulateur de la console ; la fenêtre se réduit pendant la partie et revient à la fermeture.
  Le **temps de jeu** est enregistré.
- **Installation automatique** des émulateurs (dernière release officielle, SHA-256 vérifié quand il est publié)
  dans `~/.emunest/emulators/`, avec barre de progression. Les émulateurs déjà installés se déclarent dans `config.toml`.
- **Favoris**, **récents**, **filtre par console**, **recherche**.
- **Fichiers à fournir toi-même** (BIOS, clés) : un écran t'indique où les poser. EmuNest ne télécharge **jamais**
  de BIOS ni de ROM.
- **Manette** détectée (Xbox, PlayStation, Nintendo) avec les vrais symboles des boutons.

Émulateurs pris en charge : Dolphin (GameCube, Wii), PCSX2 (PS2), DuckStation (PS1), PPSSPP (PSP), mGBA (GB/GBC/GBA),
Ryujinx (Switch, manuel), Cemu (Wii U), RPCS3 (PS3), melonDS (DS), Azahar (3DS). Un profil = un petit fichier TOML dans
`emulators/profiles/`.

## Utilisation

```powershell
cargo run --release -p emunest -- [--windowed] [dossier_de_roms]
```

Sans argument, EmuNest utilise le dossier `roms` de `~/.emunest/config.toml` :

```toml
roms = 'D:\Jeux\Emulator'

[emulators]            # facultatif : émulateurs déjà installés ailleurs
PCSX2 = 'D:\PCSX2\pcsx2-qt.exe'
```

Les données (bibliothèque, jaquettes, émulateurs installés) sont dans `~/.emunest` (`EMUNEST_DATA` pour changer).

### Touches

| Action | Clavier | Manette |
|---|---|---|
| Naviguer | ← → | croix / stick gauche |
| Jouer | Entrée | A (× sur PlayStation) |
| Changer de console | Tab / Maj+Tab | LB / RB |
| Options du jeu | O | Select |
| Favori | F | Y (△) |
| Rechercher | `/` puis taper | |
| Quitter / effacer le filtre | Échap | |

## Développement

Workspace Cargo : `core` (scan, SQLite, lancement, installation), `emulators` (profils), `metadata` (jaquettes),
`ui` (Slint), `app` (binaire). `cargo test --workspace` ; `tools/make-icon` régénère l'icône depuis `logo/emunest.svg`.
