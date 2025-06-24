// Tauri 集成示例
use stowr_core::{Config, StorageManager, create_index, FileEntry};
use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    pub compressed_size: u64,
    pub created_at: String,
    pub compression_ratio: f64,
}

impl From<FileEntry> for FileInfo {
    fn from(entry: FileEntry) -> Self {
        let compression_ratio = if entry.file_size > 0 {
            (entry.compressed_size as f64 / entry.file_size as f64) * 100.0
        } else {
            0.0
        };
        
        Self {
            path: entry.original_path.to_string_lossy().to_string(),
            size: entry.file_size,
            compressed_size: entry.compressed_size,
            created_at: entry.created_at,
            compression_ratio,
        }
    }
}

// Tauri 命令实现
pub struct StorageService {
    storage: StorageManager,
}

impl StorageService {
    pub fn new() -> anyhow::Result<Self> {
        let config = Config::default();
        let index = create_index(&config)?;
        let storage = StorageManager::new(config, index);
        
        Ok(Self { storage })
    }
    
    // Tauri 命令：存储文件
    pub fn store_file(&mut self, file_path: String, delete_source: bool) -> Result<String, String> {
        self.storage
            .store_file(Path::new(&file_path), delete_source)
            .map_err(|e| e.to_string())?;
        
        Ok(format!("File '{}' stored successfully", file_path))
    }
    
    // Tauri 命令：提取文件
    pub fn extract_file(&mut self, file_path: String) -> Result<String, String> {
        self.storage
            .owe_file(Path::new(&file_path))
            .map_err(|e| e.to_string())?;
        
        Ok(format!("File '{}' extracted successfully", file_path))
    }
    
    // Tauri 命令：列出所有文件
    pub fn list_files(&self) -> Result<Vec<FileInfo>, String> {
        let files = self.storage
            .list_files()
            .map_err(|e| e.to_string())?;
        
        Ok(files.into_iter().map(FileInfo::from).collect())
    }
    
    // Tauri 命令：搜索文件
    pub fn search_files(&self, pattern: String) -> Result<Vec<FileInfo>, String> {
        let files = self.storage
            .search_files(&pattern)
            .map_err(|e| e.to_string())?;
        
        Ok(files.into_iter().map(FileInfo::from).collect())
    }
    
    // Tauri 命令：删除文件
    pub fn delete_file(&mut self, file_path: String) -> Result<String, String> {
        self.storage
            .delete_file(Path::new(&file_path))
            .map_err(|e| e.to_string())?;
        
        Ok(format!("File '{}' deleted successfully", file_path))
    }
    
    // Tauri 命令：重命名文件
    pub fn rename_file(&mut self, old_path: String, new_path: String) -> Result<String, String> {
        self.storage
            .rename_file(Path::new(&old_path), Path::new(&new_path))
            .map_err(|e| e.to_string())?;
        
        Ok(format!("File renamed from '{}' to '{}'", old_path, new_path))
    }
    
    // Tauri 命令：移动文件
    pub fn move_file(&mut self, file_path: String, new_location: String) -> Result<String, String> {
        self.storage
            .move_file(Path::new(&file_path), Path::new(&new_location))
            .map_err(|e| e.to_string())?;
        
        Ok(format!("File '{}' moved to '{}'", file_path, new_location))
    }
}

// 如果在实际的 Tauri 应用中，会这样使用：
/*
use tauri::State;
use std::sync::Mutex;

type StorageState = Mutex<StorageService>;

#[tauri::command]
async fn store_file(
    state: State<'_, StorageState>,
    file_path: String,
    delete_source: bool,
) -> Result<String, String> {
    let mut storage = state.lock().unwrap();
    storage.store_file(file_path, delete_source)
}

#[tauri::command]
async fn list_files(state: State<'_, StorageState>) -> Result<Vec<FileInfo>, String> {
    let storage = state.lock().unwrap();
    storage.list_files()
}

// 在 main.rs 中
fn main() {
    let storage_service = StorageService::new().expect("Failed to initialize storage service");
    
    tauri::Builder::default()
        .manage(StorageState::from(Mutex::new(storage_service)))
        .invoke_handler(tauri::generate_handler![
            store_file,
            list_files,
            // ... 其他命令
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
*/

fn main() {
    println!("This is a Tauri integration example for stowr-core");
    println!("See the commented code for actual Tauri integration");
}
