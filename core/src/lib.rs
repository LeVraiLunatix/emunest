//! Modèles, scan des ROMs et bibliothèque SQLite.
pub mod backup;
pub mod config;
pub mod gallery;
pub mod install;
pub mod launch;
pub mod update;

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
    /// émulateur utilisé : celui choisi pour ce jeu, sinon celui du profil de la console
    pub emulator: String,
    /// émulateur choisi à la main pour ce jeu (None = par défaut)
    pub emu_override: Option<String>,
    /// arguments ajoutés à la ligne de commande de l'émulateur
    pub extra_args: String,
    pub note: String,
    pub collections: Vec<String>,
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
    // « Super_Mario_Galaxy_010099C022B96000v0 » (nom de dump Switch) : on retire l'identifiant de titre et la version
    let mut flat = flat.replace('_', " ");
    if let Some(i) = flat.rfind(' ') {
        let id = flat[i + 1..].to_lowercase();
        if let Some((hex, ver)) = id.split_once('v') {
            if hex.len() == 16 && hex.starts_with("01") && hex.chars().all(|c| c.is_ascii_hexdigit()) && ver.chars().all(|c| c.is_ascii_digit()) {
                flat.truncate(i);
            }
        }
    }
    let mut words: Vec<&str> = flat.split_whitespace().collect();
    // "Super Mario Odyssey v1.3.0_" : une version collée au nom brouille la recherche de jaquette
    if words.len() > 1 && words.last().is_some_and(|w| w.strip_prefix('v').is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()) && v.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '_'))) {
        words.pop();
    }
    let t = words.join(" ");
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
        for (col, def) in [
            ("favorite", "INTEGER NOT NULL DEFAULT 0"),
            ("last_played", "INTEGER NOT NULL DEFAULT 0"),
            ("play_secs", "INTEGER NOT NULL DEFAULT 0"),
            ("note", "TEXT NOT NULL DEFAULT ''"),
            ("emu_override", "TEXT"),
            ("extra_args", "TEXT NOT NULL DEFAULT ''"),
        ] {
            let has: bool = db.prepare("SELECT 1 FROM pragma_table_info('games') WHERE name=?1")?.exists([col])?;
            if !has {
                db.execute_batch(&format!("ALTER TABLE games ADD COLUMN {col} {def};"))?;
            }
        }
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS collections (name TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS collection_games (
                collection TEXT NOT NULL, path TEXT NOT NULL, PRIMARY KEY (collection, path));",
        )?;
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
        let mut memberships: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        {
            let mut st = self.db.prepare("SELECT path, collection FROM collection_games ORDER BY collection COLLATE NOCASE")?;
            for row in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
                let (path, c) = row?;
                memberships.entry(path).or_default().push(c);
            }
        }
        let mut st = self.db.prepare(
            "SELECT path,title,platform,COALESCE(emu_override, emulator),emu_override,extra_args,note,
                    favorite,last_played,play_secs FROM games ORDER BY title COLLATE NOCASE",
        )?;
        let rows = st.query_map([], |r| {
            let path: String = r.get(0)?;
            Ok(Game {
                collections: memberships.get(&path).cloned().unwrap_or_default(),
                path: path.into(),
                title: r.get(1)?,
                platform: r.get(2)?,
                emulator: r.get(3)?,
                emu_override: r.get(4)?,
                extra_args: r.get(5)?,
                note: r.get(6)?,
                favorite: r.get::<_, i64>(7)? != 0,
                last_played: r.get(8)?,
                play_secs: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Noms de toutes les collections (même vides).
    pub fn collections(&self) -> Result<Vec<String>> {
        let mut st = self.db.prepare("SELECT name FROM collections ORDER BY name COLLATE NOCASE")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn create_collection(&self, name: &str) -> Result<()> {
        self.db.execute("INSERT OR IGNORE INTO collections (name) VALUES (?1)", [name.trim()])?;
        Ok(())
    }

    pub fn delete_collection(&self, name: &str) -> Result<()> {
        self.db.execute("DELETE FROM collection_games WHERE collection=?1", [name])?;
        self.db.execute("DELETE FROM collections WHERE name=?1", [name])?;
        Ok(())
    }

    pub fn add_to_collection(&self, name: &str, path: &Path) -> Result<()> {
        self.create_collection(name)?;
        self.db.execute(
            "INSERT OR IGNORE INTO collection_games (collection, path) VALUES (?1, ?2)",
            params![name.trim(), path.to_string_lossy()],
        )?;
        Ok(())
    }

    pub fn remove_from_collection(&self, name: &str, path: &Path) -> Result<()> {
        self.db.execute("DELETE FROM collection_games WHERE collection=?1 AND path=?2", params![name, path.to_string_lossy()])?;
        Ok(())
    }

    pub fn set_note(&self, path: &Path, note: &str) -> Result<()> {
        self.db.execute("UPDATE games SET note=?2 WHERE path=?1", params![path.to_string_lossy(), note.trim()])?;
        Ok(())
    }

    /// Émulateur choisi pour ce jeu (None = revenir à celui de la console).
    pub fn set_emulator(&self, path: &Path, emulator: Option<&str>) -> Result<()> {
        self.db.execute("UPDATE games SET emu_override=?2 WHERE path=?1", params![path.to_string_lossy(), emulator])?;
        Ok(())
    }

    pub fn set_extra_args(&self, path: &Path, args: &str) -> Result<()> {
        self.db.execute("UPDATE games SET extra_args=?2 WHERE path=?1", params![path.to_string_lossy(), args.trim()])?;
        Ok(())
    }

    /// Oublie les jeux situés sous `dir` (dossier de ROMs retiré des réglages).
    pub fn remove_under(&self, dir: &Path) -> Result<()> {
        let prefix = dir.to_string_lossy().into_owned();
        let n = prefix.chars().count() as i64;
        for table in ["games", "collection_games"] {
            self.db.execute(&format!("DELETE FROM {table} WHERE substr(path, 1, ?2) = ?1"), params![prefix, n])?;
        }
        Ok(())
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
    fn cleans_switch_dump_names() {
        assert_eq!(clean_title("Super_Mario_Galaxy_2_0100FD8022DAA000v0"), "Super Mario Galaxy 2");
        assert_eq!(clean_title("Super_Mario_3D_All-Stars"), "Super Mario 3D All-Stars");
        assert_eq!(clean_title("[ROMsNS] Luigis Mansion 3 [0100DCA0064A6000][US][v0]"), "Luigis Mansion 3");
    }

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

    #[test]
    fn collections_notes_and_overrides() {
        let root = std::env::temp_dir().join(format!("emunest-coll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("ps2")).unwrap();
        std::fs::write(root.join("ps2/Game A.iso"), b"x").unwrap();
        let profiles = emunest_emulators::builtin_profiles().unwrap();
        let mut lib = Library::open_in_memory().unwrap();
        lib.scan(&root, &profiles).unwrap();
        let rom = lib.games().unwrap()[0].path.clone();

        lib.add_to_collection("À finir", &rom).unwrap();
        lib.create_collection("Vide").unwrap();
        lib.set_note(&rom, "  chapitre 3  ").unwrap();
        lib.set_emulator(&rom, Some("RPCS3")).unwrap();
        lib.set_extra_args(&rom, "--nogui").unwrap();
        lib.scan(&root, &profiles).unwrap(); // un rescan ne doit rien effacer
        let g = &lib.games().unwrap()[0];
        assert_eq!(g.collections, vec!["À finir".to_string()]);
        assert_eq!((g.note.as_str(), g.emulator.as_str(), g.extra_args.as_str()), ("chapitre 3", "RPCS3", "--nogui"));
        let mut names = lib.collections().unwrap();
        names.sort();
        assert_eq!(names, vec!["Vide".to_string(), "À finir".to_string()]);

        lib.set_emulator(&rom, None).unwrap();
        lib.remove_from_collection("À finir", &rom).unwrap();
        lib.delete_collection("Vide").unwrap();
        let g = &lib.games().unwrap()[0];
        assert_eq!(g.emulator, "PCSX2");
        assert!(g.collections.is_empty());
        assert_eq!(lib.collections().unwrap(), vec!["À finir".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
