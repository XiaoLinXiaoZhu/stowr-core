use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CompressionAlgorithm {
    Gzip,
    Zstd,
    Lz4,
}

impl Default for CompressionAlgorithm {
    fn default() -> Self {
        CompressionAlgorithm::Gzip
    }
}

impl CompressionAlgorithm {
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "gzip" => Ok(CompressionAlgorithm::Gzip),
            "zstd" => Ok(CompressionAlgorithm::Zstd),
            "lz4" => Ok(CompressionAlgorithm::Lz4),
            _ => Err(anyhow::anyhow!("Invalid compression algorithm. Valid values: gzip, zstd, lz4")),
        }
    }

    pub fn to_string(&self) -> String {
        match self {
            CompressionAlgorithm::Gzip => "gzip".to_string(),
            CompressionAlgorithm::Zstd => "zstd".to_string(),
            CompressionAlgorithm::Lz4 => "lz4".to_string(),
        }
    }

    pub fn file_extension(&self) -> &'static str {
        match self {
            CompressionAlgorithm::Gzip => "gz",
            CompressionAlgorithm::Zstd => "zst",
            CompressionAlgorithm::Lz4 => "lz4",
        }
    }

    pub fn validate_level(&self, level: u32) -> Result<u32> {
        match self {
            CompressionAlgorithm::Gzip => {
                if level > 9 {
                    Err(anyhow::anyhow!("Gzip compression level must be between 0-9"))
                } else {
                    Ok(level)
                }
            }
            CompressionAlgorithm::Zstd => {
                if level < 1 || level > 22 {
                    Err(anyhow::anyhow!("Zstd compression level must be between 1-22"))
                } else {
                    Ok(level)
                }
            }
            CompressionAlgorithm::Lz4 => {
                // LZ4 不使用压缩级别，始终返回0
                Ok(0)
            }
        }
    }

    pub fn default_level(&self) -> u32 {
        match self {
            CompressionAlgorithm::Gzip => 6,
            CompressionAlgorithm::Zstd => 3,
            CompressionAlgorithm::Lz4 => 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub storage_path: PathBuf,
    pub index_mode: IndexMode,
    #[serde(default = "default_multithread")]
    pub multithread: usize,
    #[serde(default = "default_compression_algorithm")]
    pub compression_algorithm: CompressionAlgorithm,
    #[serde(default = "default_compression_level")]
    pub compression_level: u32,
}

fn default_multithread() -> usize {
    1
}

fn default_compression_algorithm() -> CompressionAlgorithm {
    CompressionAlgorithm::Gzip
}

fn default_compression_level() -> u32 {
    6  // gzip 的默认压缩级别
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IndexMode {
    Auto,
    Json,
    Sqlite,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            storage_path: PathBuf::from(".stowr").join("storage"),
            index_mode: IndexMode::Auto,
            multithread: 1,
            compression_algorithm: CompressionAlgorithm::Gzip,
            compression_level: 6,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;
        
        if config_path.exists() {
            let content = fs::read_to_string(&config_path)
                .context("Failed to read config file")?;
            let config: Config = serde_json::from_str(&content)
                .context("Failed to parse config file")?;
            Ok(config)
        } else {
            let config = Config::default();
            config.save()?;
            Ok(config)
        }
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;
        
        // 确保配置目录存在
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create config directory")?;
        }

        // 确保存储目录存在
        fs::create_dir_all(&self.storage_path)
            .context("Failed to create storage directory")?;

        let content = serde_json::to_string_pretty(self)
            .context("Failed to serialize config")?;
        
        fs::write(&config_path, content)
            .context("Failed to write config file")?;

        Ok(())
    }

    pub fn config_path() -> Result<PathBuf> {
        Ok(PathBuf::from(".stowr").join("config.json"))
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "storage.path" => {
                self.storage_path = PathBuf::from(value);
            }
            "index.mode" => {
                self.index_mode = match value.to_lowercase().as_str() {
                    "auto" => IndexMode::Auto,
                    "json" => IndexMode::Json,
                    "sqlite" => IndexMode::Sqlite,
                    _ => return Err(anyhow::anyhow!("Invalid index mode. Valid values: auto, json, sqlite")),
                };
            }
            "multithread" => {
                self.multithread = value.parse::<usize>()
                    .map_err(|_| anyhow::anyhow!("Invalid multithread value. Must be a positive number"))?;
                if self.multithread == 0 {
                    return Err(anyhow::anyhow!("Multithread value must be greater than 0"));
                }
            }
            "compression.algorithm" => {
                self.compression_algorithm = CompressionAlgorithm::from_str(value)?;
                // 当算法改变时，更新为该算法的默认压缩级别
                self.compression_level = self.compression_algorithm.default_level();
            }
            "compression.level" => {
                let level = value.parse::<u32>()
                    .map_err(|_| anyhow::anyhow!("Invalid compression level. Must be a number"))?;
                
                // 对于LZ4，直接设置为0并提示用户
                if self.compression_algorithm == CompressionAlgorithm::Lz4 {
                    println!("Note: LZ4 does not use compression levels. Level set to 0.");
                    self.compression_level = 0;
                } else {
                    self.compression_level = self.compression_algorithm.validate_level(level)?;
                }
            }
            _ => return Err(anyhow::anyhow!("Unknown config key: {}", key)),
        }
        Ok(())
    }

    pub fn list(&self) -> Vec<(String, String)> {
        vec![
            ("storage.path".to_string(), self.storage_path.display().to_string()),
            ("index.mode".to_string(), format!("{:?}", self.index_mode).to_lowercase()),
            ("multithread".to_string(), self.multithread.to_string()),
            ("compression.algorithm".to_string(), self.compression_algorithm.to_string()),
            ("compression.level".to_string(), self.compression_level.to_string()),
        ]
    }
}
