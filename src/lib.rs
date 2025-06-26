//! # Stowr Core
//! 
//! A Rust library for file management with compression, storage, and indexing capabilities.
//! 
//! ## Quick Start
//! 
//! ```rust
//! use stowr_core::{Config, StorageManager, create_index};
//! use std::path::Path;
//! 
//! # fn main() -> anyhow::Result<()> {
//! let config = Config::default();
//! let index = create_index(&config)?;
//! let mut storage = StorageManager::new(config, index);
//! 
//! // Store a file
//! // storage.store_file(Path::new("example.txt"), false)?;
//! 
//! // List files
//! let files = storage.list_files()?;
//! println!("Stored {} files", files.len());
//! # Ok(())
//! # }
//! ```
//! 
//! ## Features
//! 
//! - File compression using gzip
//! - Dual indexing system (JSON/SQLite)
//! - Batch operations with glob patterns
//! - Multi-threaded processing
//! - Configurable compression levels
//! 
//! ## Integration
//! 
//! This library can be easily integrated into:
//! - Command-line applications
//! - Desktop applications (e.g., Tauri)
//! - Web services
//! - System utilities

pub mod config;
pub mod storage;
pub mod index;
pub mod dedup;
pub mod delta;

pub use config::{Config, IndexMode, CompressionAlgorithm, DeltaAlgorithm};
pub use storage::StorageManager;
pub use index::{FileEntry, IndexStore, create_index};
pub use dedup::{ContentDeduplicator, DedupInfo, DedupStats};
pub use delta::{DeltaStorage, DeltaInfo, SimilarityMatch, DeltaStats};

// Re-export commonly used types
pub use anyhow::Result;
pub use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lib_exports() {
        // Basic test to ensure exports work
        let _config = Config::default();
    }
}
