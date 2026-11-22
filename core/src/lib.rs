//! Modèles, scan des ROMs et bibliothèque SQLite.
pub mod install;
pub mod launch;

use anyhow::{Context, Result};
use emunest_emulators::Profile;
use rusqlite::{params, Connection};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq)]
pub struct Game {
    pub path: PathBuf,
    pub title: String,
    /// id de plateforme ("gc", "ps2"…)
    pub platform: String,
    /// nom du profil d'émulateur associé
    pub emulator: String,
    pub favorite: bool,
    /// dernière partie (secondes Unix), 0 = jamais jouée
    pub last_played: i64,
    /// temps de jeu cumulé en secondes
    pub play_secs: i64,
}

/// Extension -> (plateforme, émulateur). Si plusieurs profils partagent l'extension (ex. iso),
/// un dossier parent nommé comme une plateforme (roms/ps2/jeu.iso) tranche ; sinon le premier profil gagne.
fn resolve<'a>(path: &Path, profiles: &'a [Profile]) -> Option<(String, &'a Profile)> {
    let ext = path.extension()?.to_str()?.to_lowercase();
    let mut candidates = profiles.iter().filter(|p| p.extensions.contains(&ext));
    let first = candidates.next()?;
    let hint = path
        .ancestors()
        .skip(1)
        .filter_map(|a| a.file_name()?.to_str().map(str::to_lowercase))
        .find(|dir| profiles.iter().any(|p| p.extensions.contains(&ext) && p.platforms.contains(dir)));
    if let Some(dir) = hint {
        let p = profiles.iter().find(|p| p.extensions.contains(&ext) && p.platforms.contains(&dir))?;
        return Some((dir, p));
    }
    // Pas d'indice : plateforme = l'extension si c'en est une pour ce profil, sinon la première.
    let platform = if first.platforms.contains(&ext) { ext } else { first.platforms[0].clone() };
    Some((platform, first))
}

/// "[ROMsNS] Super Game (Europe) [!].iso" -> "Super Game" (retire tous les groupes (…) et […])
fn clean_title(stem: &str) -> String {
    let mut depth = 0;
    let flat: String = stem
        .chars()
        .filter(|&c| match c {
            '(' | '[' => {
                depth += 1;
                false
            }
            ')' | ']' => {
                depth = (depth - 1).max(0);
                false
            }
            _ => depth == 0,
        })
        .collect();
    let t = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() { stem.trim().to_string() } else { t }
}

pub struct Library {
    db: Connection,
}

impl Library {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path).context("ouverture de la base")?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> Result<Self> {
        // plusieurs connexions (scan, partie en cours, interface) : on attend au lieu d'échouer sur un verrou
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS games (
                path TEXT PRIMARY KEY, title TEXT NOT NULL,
                platform TEXT NOT NULL, emulator TEXT NOT NULL);",
        )?;
        // migration des bases créées avant les favoris et les stats
        for (col, def) in [("favorite", "INTEGER NOT NULL DEFAULT 0"), ("last_played", "INTEGER NOT NULL DEFAULT 0"), ("play_secs", "INTEGER NOT NULL DEFAULT 0")] {
            let has: bool = db.prepare("SELECT 1 FROM pragma_table_info('games') WHERE name=?1")?.exists([col])?;
            if !has {
                db.execute_batch(&format!("ALTER TABLE games ADD COLUMN {col} {def};"))?;
            }
        }
        Ok(Self { db })
    }

    /// Scanne `root`, met la base à jour (ajouts, retraits) et renvoie le nombre de jeux trouvés.
    pub fn scan(&mut self, root: &Path, profiles: &[Profile]) -> Result<usize> {
        let mut seen = HashSet::new();
        let tx = self.db.transaction()?;
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()).filter(|e| e.file_type().is_file()) {
            let path = entry.path();
            let Some((platform, profile)) = resolve(path, profiles) else { continue };
            let key = path.to_string_lossy().into_owned();
            let title = clean_title(path.file_stem().and_then(|s| s.to_str()).unwrap_or_default());
            tx.execute(
                "INSERT INTO games (path,title,platform,emulator) VALUES (?1,?2,?3,?4)
                 ON CONFLICT(path) DO UPDATE SET title=?2, platform=?3, emulator=?4",
                params![key, title, platform, profile.name],
            )?;
            seen.insert(key);
        }
        // Retire les jeux de ce dossier qui n'existent plus.
        let prefix = format!("{}%", root.to_string_lossy());
        let stale: Vec<String> = {
            let mut st = tx.prepare("SELECT path FROM games WHERE path LIKE ?1")?;
            let rows = st.query_map([prefix], |r| r.get::<_, String>(0))?;
            rows.filter_map(|r| r.ok()).filter(|p| !seen.contains(p)).collect()
        };
        for p in stale {
            tx.execute("DELETE FROM games WHERE path=?1", [p])?;
        }
        tx.commit()?;
        Ok(seen.len())
    }

    pub fn games(&self) -> Result<Vec<Game>> {
        let mut st = self.db.prepare(
            "SELECT path,title,platform,emulator,favorite,last_played,play_secs FROM games ORDER BY title COLLATE NOCASE",
        )?;
        let rows = st.query_map([], |r| {
            Ok(Game {
                path: r.get::<_, String>(0)?.into(),
                title: r.get(1)?,
                platform: r.get(2)?,
                emulator: r.get(3)?,
                favorite: r.get::<_, i64>(4)? != 0,
                last_played: r.get(5)?,
                play_secs: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_favorite(&self, path: &Path, favorite: bool) -> Result<()> {
        self.db.execute("UPDATE games SET favorite=?2 WHERE path=?1", params![path.to_string_lossy(), favorite as i64])?;
        Ok(())
    }

    /// Enregistre une partie terminée : date de dernier lancement et temps cumulé.
    pub fn record_play(&self, path: &Path, secs: i64) -> Result<()> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        self.db.execute(
            "UPDATE games SET last_played=?2, play_secs=play_secs+?3 WHERE path=?1",
            params![path.to_string_lossy(), now, secs.max(0)],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_associates_and_prunes() {
        let root = std::env::temp_dir().join(format!("emunest-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("ps2")).unwrap();
        std::fs::create_dir_all(root.join("psp")).unwrap();
        std::fs::write(root.join("ps2/Game A (Europe).iso"), b"x").unwrap();
        std::fs::write(root.join("psp/Game B.iso"), b"x").unwrap();
        std::fs::write(root.join("notes.txt"), b"x").unwrap();

        let profiles = emunest_emulators::builtin_profiles().unwrap();
        let mut lib = Library::open_in_memory().unwrap();
        assert_eq!(lib.scan(&root, &profiles).unwrap(), 2);
        let g = lib.games().unwrap();
        assert_eq!((g[0].title.as_str(), g[0].platform.as_str(), g[0].emulator.as_str()), ("Game A", "ps2", "PCSX2"));
        assert_eq!((g[1].platform.as_str(), g[1].emulator.as_str()), ("psp", "PPSSPP"));

        std::fs::remove_file(root.join("psp/Game B.iso")).unwrap();
        assert_eq!(lib.scan(&root, &profiles).unwrap(), 1);
        assert_eq!(lib.games().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn favorites_and_play_stats_survive_rescan() {
        let root = std::env::temp_dir().join(format!("emunest-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("ps2")).unwrap();
        let rom = root.join("ps2/Game A.iso");
        std::fs::write(&rom, b"x").unwrap();
        let profiles = emunest_emulators::builtin_profiles().unwrap();
        let mut lib = Library::open_in_memory().unwrap();
        lib.scan(&root, &profiles).unwrap();
        let rom = lib.games().unwrap()[0].path.clone(); // chemin tel que stocké (séparateurs normalisés)

        lib.set_favorite(&rom, true).unwrap();
        lib.record_play(&rom, 90).unwrap();
        lib.record_play(&rom, 30).unwrap();
        lib.scan(&root, &profiles).unwrap(); // un rescan ne doit rien effacer
        let g = &lib.games().unwrap()[0];
        assert!(g.favorite && g.play_secs == 120 && g.last_played > 0);
        let _ = std::fs::remove_dir_all(&root);
    }
}
