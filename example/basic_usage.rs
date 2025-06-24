// 基本使用示例
use stowr_core::{Config, StorageManager, create_index};
use std::path::Path;
use anyhow::Result;

fn main() -> Result<()> {
    println!("Stowr Core Library Basic Example");
    
    // 创建配置
    let mut config = Config::default();
    config.storage_path = Path::new("./example_storage").to_path_buf();
    
    // 创建索引
    let index = create_index(&config)?;
    
    // 创建存储管理器
    let mut storage = StorageManager::new(config, index);
    
    // 创建一个示例文件
    std::fs::write("example.txt", "Hello, Stowr!")?;
    
    // 存储文件
    println!("Storing file: example.txt");
    storage.store_file(Path::new("example.txt"), false)?;
    
    // 列出所有存储的文件
    println!("\nStored files:");
    let files = storage.list_files()?;
    for file in &files {
        println!("- {} ({} bytes -> {} bytes, {:.1}% compression)", 
                 file.original_path.display(),
                 file.file_size,
                 file.compressed_size,
                 (file.compressed_size as f64 / file.file_size as f64) * 100.0);
    }
    
    // 搜索文件
    println!("\nSearching for *.txt files:");
    let search_results = storage.search_files("*.txt")?;
    for file in search_results {
        println!("Found: {}", file.original_path.display());
    }
    
    // 提取文件到新位置
    println!("\nExtracting file to extracted_example.txt");
    storage.owe_file(Path::new("example.txt"))?;
    
    // 清理
    let _ = std::fs::remove_file("example.txt");
    let _ = std::fs::remove_file("extracted_example.txt");
    let _ = std::fs::remove_dir_all("example_storage");
    let _ = std::fs::remove_dir(".stowr");
    
    println!("Example completed successfully!");
    Ok(())
}
