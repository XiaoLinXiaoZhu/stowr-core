use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{Config, IndexMode};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub id: String,
    pub original_path: PathBuf,
    pub stored_path: PathBuf,
    pub file_size: u64,
    pub compressed_size: u64,
    pub created_at: String,
}

pub trait IndexStore {
    fn add_file(&mut self, entry: FileEntry) -> Result<()>;
    fn get_file(&self, original_path: &Path) -> Result<Option<FileEntry>>;
    fn remove_file(&mut self, original_path: &Path) -> Result<Option<FileEntry>>;
    fn list_files(&self) -> Result<Vec<FileEntry>>;
    fn rename_file(&mut self, old_path: &Path, new_path: &Path) -> Result<()>;
    fn move_file(&mut self, original_path: &Path, new_path: &Path) -> Result<()>;
    fn count(&self) -> Result<usize>;
}

pub struct JsonIndex {
    index_path: PathBuf,
    entries: HashMap<PathBuf, FileEntry>,
}

impl JsonIndex {
    pub fn new(storage_path: &Path) -> Result<Self> {
        let index_path = storage_path.join("index.json");
        let entries = if index_path.exists() {
            let content = fs::read_to_string(&index_path)
                .context("Failed to read index file")?;
            serde_json::from_str(&content)
                .unwrap_or_else(|_| HashMap::new())
        } else {
            HashMap::new()
        };

        Ok(Self {
            index_path,
            entries,
        })
    }

    fn save(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(&self.entries)
            .context("Failed to serialize index")?;
        fs::write(&self.index_path, content)
            .context("Failed to write index file")?;
        Ok(())
    }
}

impl IndexStore for JsonIndex {
    fn add_file(&mut self, entry: FileEntry) -> Result<()> {
        self.entries.insert(entry.original_path.clone(), entry);
        self.save()
    }

    fn get_file(&self, original_path: &Path) -> Result<Option<FileEntry>> {
        Ok(self.entries.get(original_path).cloned())
    }

    fn remove_file(&mut self, original_path: &Path) -> Result<Option<FileEntry>> {
        let entry = self.entries.remove(original_path);
        self.save()?;
        Ok(entry)
    }

    fn list_files(&self) -> Result<Vec<FileEntry>> {
        Ok(self.entries.values().cloned().collect())
    }

    fn rename_file(&mut self, old_path: &Path, new_path: &Path) -> Result<()> {
        if let Some(mut entry) = self.entries.remove(old_path) {
            entry.original_path = new_path.to_path_buf();
            self.entries.insert(new_path.to_path_buf(), entry);
            self.save()?;
        }
        Ok(())
    }

    fn move_file(&mut self, original_path: &Path, new_path: &Path) -> Result<()> {
        if let Some(mut entry) = self.entries.remove(original_path) {
            entry.original_path = new_path.to_path_buf();
            self.entries.insert(new_path.to_path_buf(), entry);
            self.save()?;
        }
        Ok(())
    }

    fn count(&self) -> Result<usize> {
        Ok(self.entries.len())
    }
}

pub struct SqliteIndex {
    conn: Connection,
}

impl SqliteIndex {
    pub fn new(storage_path: &Path) -> Result<Self> {
        let db_path = storage_path.join("index.db");
        let conn = Connection::open(db_path)
            .context("Failed to open SQLite database")?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS files (
                original_path TEXT PRIMARY KEY,
                id TEXT NOT NULL,
                stored_path TEXT NOT NULL,
                file_size INTEGER NOT NULL,
                compressed_size INTEGER NOT NULL,
                created_at TEXT NOT NULL
            )",
            [],
        )?;

        Ok(Self { conn })
    }
}

impl IndexStore for SqliteIndex {
    fn add_file(&mut self, entry: FileEntry) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO files (original_path, id, stored_path, file_size, compressed_size, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                entry.original_path.to_string_lossy(),
                entry.id,
                entry.stored_path.to_string_lossy(),
                entry.file_size,
                entry.compressed_size,
                entry.created_at
            ],
        )?;
        Ok(())
    }

    fn get_file(&self, original_path: &Path) -> Result<Option<FileEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, stored_path, file_size, compressed_size, created_at 
             FROM files WHERE original_path = ?1"
        )?;

        let entry = stmt.query_row([original_path.to_string_lossy()], |row| {
            Ok(FileEntry {
                id: row.get(0)?,
                original_path: original_path.to_path_buf(),
                stored_path: PathBuf::from(row.get::<_, String>(1)?),
                file_size: row.get(2)?,
                compressed_size: row.get(3)?,
                created_at: row.get(4)?,
            })
        }).optional()?;

        Ok(entry)
    }

    fn remove_file(&mut self, original_path: &Path) -> Result<Option<FileEntry>> {
        let entry = self.get_file(original_path)?;
        if entry.is_some() {
            self.conn.execute(
                "DELETE FROM files WHERE original_path = ?1",
                [original_path.to_string_lossy()],
            )?;
        }
        Ok(entry)
    }

    fn list_files(&self) -> Result<Vec<FileEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT original_path, id, stored_path, file_size, compressed_size, created_at FROM files"
        )?;

        let entries = stmt.query_map([], |row| {
            Ok(FileEntry {
                original_path: PathBuf::from(row.get::<_, String>(0)?),
                id: row.get(1)?,
                stored_path: PathBuf::from(row.get::<_, String>(2)?),
                file_size: row.get(3)?,
                compressed_size: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;

        Ok(entries)
    }

    fn rename_file(&mut self, old_path: &Path, new_path: &Path) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET original_path = ?1 WHERE original_path = ?2",
            rusqlite::params![
                new_path.to_string_lossy(),
                old_path.to_string_lossy()
            ],
        )?;
        Ok(())
    }

    fn move_file(&mut self, original_path: &Path, new_path: &Path) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET original_path = ?1 WHERE original_path = ?2",
            rusqlite::params![
                new_path.to_string_lossy(),
                original_path.to_string_lossy()
            ],
        )?;
        Ok(())
    }

    fn count(&self) -> Result<usize> {
        let mut stmt = self.conn.prepare("SELECT COUNT(*) FROM files")?;
        let count: i64 = stmt.query_row([], |row| row.get(0))?;
        Ok(count as usize)
    }
}

pub fn create_index(config: &Config) -> Result<Box<dyn IndexStore>> {
    fs::create_dir_all(&config.storage_path)?;

    let mode = match &config.index_mode {
        IndexMode::Auto => {
            // 尝试读取现有的索引来决定使用哪种模式
            let json_index = JsonIndex::new(&config.storage_path)?;
            let count = json_index.count()?;
            if count >= 1000 {
                IndexMode::Sqlite
            } else {
                IndexMode::Json
            }
        }
        mode => mode.clone(),
    };

    match mode {
        IndexMode::Json | IndexMode::Auto => {
            Ok(Box::new(JsonIndex::new(&config.storage_path)?))
        }
        IndexMode::Sqlite => {
            Ok(Box::new(SqliteIndex::new(&config.storage_path)?))
        }
    }
}
