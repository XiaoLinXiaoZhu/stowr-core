# Stowr Core

[![Crates.io](https://img.shields.io/crates/v/stowr-core.svg)](https://crates.io/crates/stowr-core)
[![Documentation](https://docs.rs/stowr-core/badge.svg)](https://docs.rs/stowr-core)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)

`stowr-core` 是一个用 Rust 编写的文件管理核心库，提供文件压缩、存储和索引功能。它可以独立使用，也可以集成到其他应用程序中。

## 为什么叫做STOWR？
STOWR是一个由“Store”和“Owe”,两个单词组合而成的名称。它能够实现动态的 压缩/解压 文件功能，实现小文件的优化存储。

当文件被存储时，它对于 STOWR 来说处于“Store”状态；而对于文件读写来说，它处于“Owe”状态。当文件被解压后，它将被从STOWR中删除，STOWR将不再拥有该文件。

当处于“Owe”状态时，文件的内容是不可见的，但是你仍然可以将其重命名、移动或删除。

想要查看文件内容，你需要首先使用 STOWR 将其提取出来，提取之后，stowr 将不再 “store” 该文件,“owe"关系也会被解除。

想想看：`stowr owe me_a_file.txt` stowr 你欠我一个文件！

## 功能特性

- **多种压缩算法**: 支持 gzip（默认）、zstd、lz4 压缩算法，可根据需求选择
- **灵活压缩级别**: 每种算法支持不同的压缩级别配置
  - gzip: 0-9（默认6）
  - zstd: 1-22（默认3）
  - lz4: 无级别配置（专注于速度）
- **智能去重存储**: 自动检测重复文件，通过内容哈希实现零冗余存储
- **差分压缩技术**: 对相似文件使用差分存储，大幅减少存储空间占用
- **双重索引系统**: 支持 JSON 和 SQLite 两种索引模式，自动选择最优方案
- **批量文件操作**: 支持通配符模式批量处理文件
- **多线程支持**: 并行处理大量文件，提升性能
- **灵活配置**: 可配置压缩算法、压缩级别、存储路径、索引模式、去重和差分策略等
- **模块化设计**: 易于集成到不同类型的应用程序中

## 快速开始

将以下依赖添加到您的 `Cargo.toml`：

```toml
[dependencies]
stowr-core = "0.2.2"
```

### 基本使用

```rust
use stowr_core::{Config, StorageManager, create_index};
use std::path::Path;

fn main() -> anyhow::Result<()> {
    // 创建配置
    let config = Config::default();
    
    // 创建索引
    let index = create_index(&config)?;
    
    // 创建存储管理器
    let mut storage = StorageManager::new(config, index);
    
    // 存储文件
    storage.store_file(Path::new("example.txt"), false)?;
    
    // 列出所有文件
    let files = storage.list_files()?;
    for file in files {
        println!("File: {} ({} bytes)", 
                 file.original_path.display(), 
                 file.file_size);
    }
    
    // 搜索文件
    let results = storage.search_files("*.txt")?;
    println!("Found {} text files", results.len());
    
    // 提取文件
    storage.owe_file(Path::new("example.txt"))?;
    
    Ok(())
}
```

### 配置选项

```rust
use stowr_core::{Config, IndexMode, CompressionAlgorithm, DeltaAlgorithm};
use std::path::PathBuf;

let mut config = Config::default();
config.storage_path = PathBuf::from("./my_storage");
config.index_mode = IndexMode::Sqlite;  // 强制使用 SQLite 索引
config.compression_algorithm = CompressionAlgorithm::Zstd;  // 使用 zstd 压缩
config.compression_level = 15;          // zstd 高压缩级别
config.multithread = 4;                 // 使用 4 个线程

// 去重和差分配置
config.enable_deduplication = true;     // 启用内容去重
config.enable_delta_compression = true; // 启用差分压缩
config.similarity_threshold = 0.8;      // 80% 相似度阈值
config.delta_algorithm = DeltaAlgorithm::Simple; // 差分算法
```

### 去重和差分存储

STOWR 提供强大的去重和差分存储功能，特别适合存储大量相似文件：

```rust
use stowr_core::{Config, StorageManager, create_index, DeltaAlgorithm};

// 配置去重和差分功能
let mut config = Config::default();
config.enable_deduplication = true;     // 启用内容去重
config.enable_delta_compression = true; // 启用差分压缩
config.similarity_threshold = 0.7;      // 70% 相似度阈值

let index = create_index(&config)?;
let mut storage = StorageManager::new(config, index);

// 存储文件 - 自动检测重复和相似文件
storage.store_file(Path::new("texture_v1.png"), false)?;
storage.store_file(Path::new("texture_v2.png"), false)?; // 可能被差分存储
storage.store_file(Path::new("texture_v1_copy.png"), false)?; // 可能被去重
```

#### 去重功能特点

- **内容哈希**: 使用 SHA256 计算文件内容哈希
- **零冗余**: 完全相同的文件只存储一份
- **引用计数**: 自动管理文件引用，安全删除
- **透明操作**: 对用户完全透明，无需额外操作

#### 差分压缩特点

- **相似度检测**: 智能检测文件间的相似性
- **多种算法**: 支持简单差分、xdelta、bsdiff 等算法
- **类型优先**: 优先与相同类型文件进行差分
- **空间节省**: 大幅减少相似文件的存储空间

### 压缩算法选择

不同的压缩算法适用于不同的场景：

- **gzip**: 通用性好，压缩率中等，速度中等（默认）
- **zstd**: 压缩率高，速度快，现代推荐选择
- **lz4**: 压缩速度极快，压缩率较低，适合实时处理

```rust
use stowr_core::{Config, CompressionAlgorithm};

// 使用不同的压缩算法
let mut config = Config::default();

// 高压缩率场景
config.compression_algorithm = CompressionAlgorithm::Zstd;
config.compression_level = 20;

// 高速度场景
config.compression_algorithm = CompressionAlgorithm::Lz4;
// lz4 无需设置压缩级别

// 兼容性场景
config.compression_algorithm = CompressionAlgorithm::Gzip;
config.compression_level = 6;
```

## 高级功能

### 批量操作

```rust
// 从文件列表批量存储
storage.store_files_from_list(Path::new("file_list.txt"), false)?;

// 批量提取
storage.owe_files_from_list(Path::new("extract_list.txt"))?;

// 提取所有文件
storage.owe_all_files()?;
```

### 文件管理

```rust
// 重命名文件
storage.rename_file(Path::new("old_name.txt"), Path::new("new_name.txt"))?;

// 移动文件
storage.move_file(Path::new("file.txt"), Path::new("new/location/"))?;

// 删除文件
storage.delete_file(Path::new("unwanted.txt"))?;
```

## 与其他框架集成

### Tauri 集成

```rust
use stowr_core::{Config, StorageManager, create_index};
use tauri::State;
use std::sync::Mutex;

type StorageState = Mutex<StorageManager>;

#[tauri::command]
async fn store_file(
    state: State<'_, StorageState>,
    file_path: String,
) -> Result<String, String> {
    let mut storage = state.lock().unwrap();
    storage.store_file(Path::new(&file_path), false)
        .map_err(|e| e.to_string())?;
    Ok("File stored successfully".to_string())
}
```

### Web 服务集成

```rust
use stowr_core::{Config, StorageManager, create_index};

pub struct FileService {
    storage: StorageManager,
}

impl FileService {
    pub fn new() -> anyhow::Result<Self> {
        let config = Config::default();
        let index = create_index(&config)?;
        let storage = StorageManager::new(config, index);
        Ok(Self { storage })
    }
    
    pub fn store_upload(&mut self, file_path: &Path) -> anyhow::Result<()> {
        self.storage.store_file(file_path, false)
    }
}
```

## 索引模式

- **Auto**: 根据文件数量自动选择（< 1000 文件使用 JSON，>= 1000 使用 SQLite）
- **Json**: 使用 JSON 文件存储索引，适合小规模使用
- **Sqlite**: 使用 SQLite 数据库存储索引，适合大规模使用

## 性能考虑

- **压缩算法选择**: 根据使用场景选择合适的压缩算法
  - gzip: 平衡的压缩率和速度，默认级别6
  - zstd: 现代高效算法，推荐用于新项目，默认级别3
  - lz4: 极速压缩，适合实时或临时存储
- 多线程处理在文件数量 > 1 且线程数 > 1 时自动启用
- SQLite 索引在大量文件时性能更好
- 内存使用量与并发线程数成正比

## 许可证

本项目采用 GPL-3.0-or-later 许可证。详见 [LICENSE](LICENSE) 文件。

## 贡献

欢迎提交 Issue 和 Pull Request。请确保在提交前运行测试：

```bash
cargo test
```

## 相关项目

- [stowr](https://crates.io/crates/stowr) - 基于 stowr-core 的命令行工具
