use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use chrono;

use crate::config::{Config, IndexMode, CompressionAlgorithm, DeltaAlgorithm};
use crate::dedup::DedupInfo;
use crate::delta::DeltaInfo;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub id: String,
    pub original_path: PathBuf,
    pub stored_path: PathBuf,
    pub file_size: u64,
    pub compressed_size: u64,
    pub created_at: String,
    pub compression_algorithm: CompressionAlgorithm,
    // 去重相关字段
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_reference: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_storage_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_count: Option<u32>,
    // 差分相关字段
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_delta: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_storage_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity_score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_algorithm: Option<DeltaAlgorithm>,
}

impl FileEntry {
    /// 创建新的文件条目
    pub fn new(
        id: String,
        original_path: PathBuf,
        stored_path: PathBuf,
        file_size: u64,
        compressed_size: u64,
        compression_algorithm: CompressionAlgorithm,
    ) -> Self {
        Self {
            id,
            original_path,
            stored_path,
            file_size,
            compressed_size,
            created_at: chrono::Utc::now().to_rfc3339(),
            compression_algorithm,
            hash: None,
            is_reference: None,
            original_storage_id: None,
            ref_count: None,
            is_delta: None,
            base_storage_id: None,
            similarity_score: None,
            delta_algorithm: None,
        }
    }

    /// 设置去重信息
    pub fn set_dedup_info(&mut self, dedup_info: DedupInfo) {
        self.hash = Some(dedup_info.hash);
        self.is_reference = Some(dedup_info.is_reference);
        self.original_storage_id = dedup_info.original_storage_id;
        self.ref_count = Some(dedup_info.ref_count);
    }

    /// 设置差分信息
    pub fn set_delta_info(&mut self, delta_info: DeltaInfo) {
        self.is_delta = Some(delta_info.is_delta);
        self.base_storage_id = delta_info.base_storage_id;
        self.similarity_score = delta_info.similarity_score;
        self.delta_algorithm = Some(delta_info.delta_algorithm);
        self.compressed_size = delta_info.delta_size;
    }

    /// 检查是否为引用文件
    pub fn is_reference_file(&self) -> bool {
        self.is_reference.unwrap_or(false)
    }

    /// 检查是否为差分文件
    pub fn is_delta_file(&self) -> bool {
        self.is_delta.unwrap_or(false)
    }

    /// 获取实际存储大小（考虑引用文件）
    pub fn get_actual_storage_size(&self) -> u64 {
        if self.is_reference_file() {
            0 // 引用文件不占用额外存储空间
        } else {
            self.compressed_size
        }
    }
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
                created_at TEXT NOT NULL,
                compression_algorithm TEXT NOT NULL DEFAULT 'gzip',
                hash TEXT,
                is_reference INTEGER DEFAULT 0,
                original_storage_id TEXT,
                ref_count INTEGER DEFAULT 1,
                is_delta INTEGER DEFAULT 0,
                base_storage_id TEXT,
                similarity_score REAL,
                delta_algorithm TEXT
            )",
            [],
        )?;

        Ok(Self { conn })
    }
}

impl IndexStore for SqliteIndex {
    fn add_file(&mut self, entry: FileEntry) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO files (
                original_path, id, stored_path, file_size, compressed_size, created_at,
                compression_algorithm, hash, is_reference, original_storage_id, ref_count,
                is_delta, base_storage_id, similarity_score, delta_algorithm
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                entry.original_path.to_string_lossy(),
                entry.id,
                entry.stored_path.to_string_lossy(),
                entry.file_size,
                entry.compressed_size,
                entry.created_at,
                entry.compression_algorithm.to_string(),
                entry.hash,
                entry.is_reference.map(|b| if b { 1 } else { 0 }),
                entry.original_storage_id,
                entry.ref_count,
                entry.is_delta.map(|b| if b { 1 } else { 0 }),
                entry.base_storage_id,
                entry.similarity_score,
                entry.delta_algorithm.as_ref().map(|a| a.to_string())
            ],
        )?;
        Ok(())
    }

    fn get_file(&self, original_path: &Path) -> Result<Option<FileEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, stored_path, file_size, compressed_size, created_at,
                    compression_algorithm, hash, is_reference, original_storage_id, ref_count,
                    is_delta, base_storage_id, similarity_score, delta_algorithm
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
                compression_algorithm: row.get::<_, String>(5)?.parse()
                    .map_err(|_| rusqlite::Error::InvalidColumnType(5, "compression_algorithm".to_string(), rusqlite::types::Type::Text))?,
                hash: row.get(6)?,
                is_reference: row.get::<_, Option<i32>>(7)?.map(|i| i != 0),
                original_storage_id: row.get(8)?,
                ref_count: row.get(9)?,
                is_delta: row.get::<_, Option<i32>>(10)?.map(|i| i != 0),
                base_storage_id: row.get(11)?,
                similarity_score: row.get(12)?,
                delta_algorithm: row.get::<_, Option<String>>(13)?
                    .map(|s| s.parse())
                    .transpose()
                    .map_err(|_| rusqlite::Error::InvalidColumnType(13, "delta_algorithm".to_string(), rusqlite::types::Type::Text))?,
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
            "SELECT original_path, id, stored_path, file_size, compressed_size, created_at,
                    compression_algorithm, hash, is_reference, original_storage_id, ref_count,
                    is_delta, base_storage_id, similarity_score, delta_algorithm
             FROM files"
        )?;

        let entries = stmt.query_map([], |row| {
            Ok(FileEntry {
                original_path: PathBuf::from(row.get::<_, String>(0)?),
                id: row.get(1)?,
                stored_path: PathBuf::from(row.get::<_, String>(2)?),
                file_size: row.get(3)?,
                compressed_size: row.get(4)?,
                created_at: row.get(5)?,
                compression_algorithm: row.get::<_, String>(6)?.parse()
                    .map_err(|_| rusqlite::Error::InvalidColumnType(6, "compression_algorithm".to_string(), rusqlite::types::Type::Text))?,
                hash: row.get(7)?,
                is_reference: row.get::<_, Option<i32>>(8)?.map(|i| i != 0),
                original_storage_id: row.get(9)?,
                ref_count: row.get(10)?,
                is_delta: row.get::<_, Option<i32>>(11)?.map(|i| i != 0),
                base_storage_id: row.get(12)?,
                similarity_score: row.get(13)?,
                delta_algorithm: row.get::<_, Option<String>>(14)?
                    .map(|s| s.parse())
                    .transpose()
                    .map_err(|_| rusqlite::Error::InvalidColumnType(14, "delta_algorithm".to_string(), rusqlite::types::Type::Text))?,
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
