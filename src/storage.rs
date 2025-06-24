use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use glob::glob;
use std::fs::{self, File};
use std::io::{self};
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::config::Config;
use crate::index::{FileEntry, IndexStore};

pub struct StorageManager {
    config: Config,
    index: Box<dyn IndexStore>,
}

impl StorageManager {
    pub fn new(config: Config, index: Box<dyn IndexStore>) -> Self {
        Self { config, index }
    }

    pub fn store_file(&mut self, file_path: &Path, delete_source: bool) -> Result<()> {
        if !file_path.exists() {
            return Err(anyhow::anyhow!("File does not exist: {}", file_path.display()));
        }

        if !file_path.is_file() {
            return Err(anyhow::anyhow!("Path is not a file: {}", file_path.display()));
        }

        // 检查文件是否已经存储
        if self.index.get_file(file_path)?.is_some() {
            println!("File already stored: {}", file_path.display());
            if delete_source {
                fs::remove_file(file_path)
                    .context("Failed to delete source file")?;
                println!("Source file deleted: {}", file_path.display());
            }
            return Ok(());
        }

        // 生成唯一ID和存储路径
        let id = Uuid::new_v4().to_string();
        let stored_filename = format!("{}.gz", id);
        let stored_path = self.config.storage_path.join(&stored_filename);

        // 确保存储目录存在
        fs::create_dir_all(&self.config.storage_path)
            .context("Failed to create storage directory")?;

        // 获取原始文件大小
        let file_size = fs::metadata(file_path)?.len();

        // 压缩并存储文件
        let compressed_size = self.compress_file(file_path, &stored_path)
            .context("Failed to compress file")?;

        // 创建索引条目
        let entry = FileEntry {
            id,
            original_path: file_path.to_path_buf(),
            stored_path,
            file_size,
            compressed_size,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        // 添加到索引
        self.index.add_file(entry)
            .context("Failed to add file to index")?;

        // 如果需要删除源文件
        if delete_source {
            fs::remove_file(file_path)
                .context("Failed to delete source file")?;
            println!("Source file deleted: {}", file_path.display());
        }

        println!("File stored successfully: {}", file_path.display());
        println!("Compression ratio: {:.1}%", 
                 (compressed_size as f64 / file_size as f64) * 100.0);

        Ok(())
    }

    pub fn owe_file(&mut self, file_path: &Path) -> Result<()> {
        let entry = self.index.get_file(file_path)?
            .ok_or_else(|| anyhow::anyhow!("File not found in storage: {}", file_path.display()))?;

        // 解压文件到原始位置
        self.decompress_file(&entry.stored_path, &entry.original_path)
            .context("Failed to decompress file")?;

        // 删除压缩的存储文件
        fs::remove_file(&entry.stored_path)
            .context("Failed to remove stored file")?;

        // 从索引中移除
        self.index.remove_file(file_path)?;

        println!("File extracted successfully: {}", file_path.display());
        Ok(())
    }

    pub fn list_files(&self) -> Result<Vec<FileEntry>> {
        self.index.list_files()
    }

    pub fn search_files(&self, pattern: &str) -> Result<Vec<FileEntry>> {
        let all_files = self.index.list_files()?;
        let mut matching_files = Vec::new();

        // 创建glob模式匹配器
        for file_entry in all_files {
            // 将路径转换为字符串进行匹配
            let path_str = file_entry.original_path.to_string_lossy();
            
            // 使用glob模式匹配
            if let Ok(matcher) = glob::Pattern::new(pattern) {
                if matcher.matches(&path_str) {
                    matching_files.push(file_entry);
                }
            } else {
                // 如果不是有效的glob模式，进行简单的字符串匹配
                if path_str.contains(pattern) {
                    matching_files.push(file_entry);
                }
            }
        }

        Ok(matching_files)
    }

    pub fn rename_file(&mut self, old_path: &Path, new_path: &Path) -> Result<()> {
        if self.index.get_file(old_path)?.is_none() {
            return Err(anyhow::anyhow!("File not found in storage: {}", old_path.display()));
        }

        if self.index.get_file(new_path)?.is_some() {
            return Err(anyhow::anyhow!("Target file already exists: {}", new_path.display()));
        }

        self.index.rename_file(old_path, new_path)
            .context("Failed to rename file in index")?;

        println!("File renamed: {} -> {}", old_path.display(), new_path.display());
        Ok(())
    }

    pub fn move_file(&mut self, file_path: &Path, new_location: &Path) -> Result<()> {
        if self.index.get_file(file_path)?.is_none() {
            return Err(anyhow::anyhow!("File not found in storage: {}", file_path.display()));
        }

        let filename = file_path.file_name()
            .ok_or_else(|| anyhow::anyhow!("Invalid file path"))?;
        let new_path = new_location.join(filename);

        if self.index.get_file(&new_path)?.is_some() {
            return Err(anyhow::anyhow!("Target file already exists: {}", new_path.display()));
        }

        self.index.move_file(file_path, &new_path)
            .context("Failed to move file in index")?;

        println!("File moved: {} -> {}", file_path.display(), new_path.display());
        Ok(())
    }

    pub fn delete_file(&mut self, file_path: &Path) -> Result<()> {
        let entry = self.index.remove_file(file_path)?
            .ok_or_else(|| anyhow::anyhow!("File not found in storage: {}", file_path.display()))?;

        // 删除存储的文件
        if entry.stored_path.exists() {
            fs::remove_file(&entry.stored_path)
                .context("Failed to remove stored file")?;
        }

        println!("File deleted from storage: {}", file_path.display());
        Ok(())
    }

    fn compress_file(&self, input_path: &Path, output_path: &Path) -> Result<u64> {
        let mut input_file = File::open(input_path)
            .context("Failed to open input file")?;
        let output_file = File::create(output_path)
            .context("Failed to create output file")?;

        // 使用配置中设置的压缩级别
        let compression_level = Compression::new(self.config.compression_level);

        let mut encoder = GzEncoder::new(output_file, compression_level);
        io::copy(&mut input_file, &mut encoder)
            .context("Failed to compress file")?;

        encoder.finish()
            .context("Failed to finalize compression")?;

        let compressed_size = fs::metadata(output_path)?.len();
        Ok(compressed_size)
    }

    fn decompress_file(&self, input_path: &Path, output_path: &Path) -> Result<()> {
        let input_file = File::open(input_path)
            .context("Failed to open compressed file")?;
        let mut decoder = GzDecoder::new(input_file);

        // 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create output directory")?;
        }

        let mut output_file = File::create(output_path)
            .context("Failed to create output file")?;

        io::copy(&mut decoder, &mut output_file)
            .context("Failed to decompress file")?;

        Ok(())
    }

    pub fn store_files_from_list(&mut self, list_file: &Path, delete_source: bool) -> Result<()> {
        let content = fs::read_to_string(list_file)
            .context("Failed to read file list")?;

        let mut include_patterns = Vec::new();
        let mut exclude_patterns = Vec::new();

        // 解析包含和排除模式
        for line in content.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                if line.starts_with('!') {
                    // 排除模式（以!开头）
                    exclude_patterns.push(&line[1..]);
                } else {
                    // 包含模式
                    include_patterns.push(line);
                }
            }
        }

        // 收集所有匹配的文件
        let mut all_files = Vec::new();
        
        for pattern in include_patterns {
            if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
                // 处理通配符模式
                match self.process_glob_pattern(pattern) {
                    Ok(files) => {
                        all_files.extend(files);
                    }
                    Err(e) => {
                        eprintln!("Failed to process glob pattern '{}': {}", pattern, e);
                    }
                }
            } else {
                // 普通文件路径
                let file_path = PathBuf::from(pattern);
                if file_path.exists() {
                    all_files.push(file_path);
                }
            }
        }

        // 应用排除模式
        let filtered_files = self.apply_exclude_patterns(all_files, &exclude_patterns)?;

        // 如果启用多线程且文件数量足够
        if self.config.multithread > 1 && filtered_files.len() > 1 {
            // 使用多线程处理
            self.store_files_parallel(filtered_files, delete_source)?;
        } else {
            // 使用单线程顺序处理
            for file_path in filtered_files {
                if let Err(e) = self.store_file(&file_path, delete_source) {
                    eprintln!("Failed to store {}: {}", file_path.display(), e);
                }
            }
        }

        Ok(())
    }

    pub fn owe_files_from_list(&mut self, list_file: &Path) -> Result<()> {
        let content = fs::read_to_string(list_file)
            .context("Failed to read file list")?;

        let mut include_patterns = Vec::new();
        let mut exclude_patterns = Vec::new();

        // 解析包含和排除模式
        for line in content.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                if line.starts_with('!') {
                    // 排除模式（以!开头）
                    exclude_patterns.push(&line[1..]);
                } else {
                    // 包含模式
                    include_patterns.push(line);
                }
            }
        }

        // 收集所有匹配的已存储文件
        let mut all_files = Vec::new();

        for pattern in include_patterns {
            if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
                // 对于owe操作，我们需要从索引中查找匹配的文件
                match self.find_stored_files_by_pattern(pattern) {
                    Ok(files) => {
                        all_files.extend(files);
                    }
                    Err(e) => {
                        eprintln!("Failed to process pattern '{}': {}", pattern, e);
                    }
                }
            } else {
                // 普通文件路径
                let file_path = PathBuf::from(pattern);
                if self.index.get_file(&file_path)?.is_some() {
                    all_files.push(file_path);
                }
            }
        }

        // 应用排除模式到已存储的文件
        let filtered_files = self.apply_exclude_patterns_to_stored(all_files, &exclude_patterns)?;

        // 如果启用多线程且文件数量足够
        if self.config.multithread > 1 && filtered_files.len() > 1 {
            // 使用多线程处理
            self.owe_files_parallel(filtered_files)?;
        } else {
            // 使用单线程顺序处理
            for file_path in filtered_files {
                if let Err(e) = self.owe_file(&file_path) {
                    eprintln!("Failed to owe {}: {}", file_path.display(), e);
                }
            }
        }

        Ok(())
    }

    /// 处理通配符模式，返回匹配的文件路径列表
    fn process_glob_pattern(&self, pattern: &str) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        
        // 使用glob crate处理通配符
        for entry in glob(pattern).context("Failed to parse glob pattern")? {
            match entry {
                Ok(path) => {
                    if path.is_file() {
                        files.push(path);
                    }
                }
                Err(e) => {
                    eprintln!("Error reading path: {}", e);
                }
            }
        }

        if files.is_empty() {
            println!("No files matched pattern: {}", pattern);
        } else {
            println!("Found {} files matching pattern: {}", files.len(), pattern);
        }

        Ok(files)
    }

    /// 在已存储的文件中查找匹配通配符模式的文件
    fn find_stored_files_by_pattern(&self, pattern: &str) -> Result<Vec<PathBuf>> {
        let stored_files = self.index.list_files()?;
        let mut matching_files = Vec::new();

        // 将通配符模式转换为正则表达式
        let regex_pattern = self.glob_to_regex(pattern)?;
        let regex = regex::Regex::new(&regex_pattern)
            .context("Failed to compile regex pattern")?;

        for entry in stored_files {
            let path_str = entry.original_path.to_string_lossy();
            if regex.is_match(&path_str) {
                matching_files.push(entry.original_path);
            }
        }

        if matching_files.is_empty() {
            println!("No stored files matched pattern: {}", pattern);
        } else {
            println!("Found {} stored files matching pattern: {}", matching_files.len(), pattern);
        }

        Ok(matching_files)
    }

    /// 将通配符模式转换为正则表达式
    pub fn glob_to_regex(&self, pattern: &str) -> Result<String> {
        let mut regex = String::new();
        let chars: Vec<char> = pattern.chars().collect();
        let mut i = 0;

        regex.push('^');

        while i < chars.len() {
            match chars[i] {
                '*' => {
                    if i + 1 < chars.len() && chars[i + 1] == '*' {
                        // ** 匹配任意深度的目录
                        regex.push_str(".*");
                        i += 1; // 跳过下一个 *
                    } else {
                        // * 匹配单个目录层级中的任意字符（不包括路径分隔符）
                        regex.push_str(r"[^/\\]*");
                    }
                }
                '?' => {
                    // ? 匹配单个字符（不包括路径分隔符）
                    regex.push_str(r"[^/\\]");
                }
                '[' => {
                    // 字符类保持原样
                    regex.push('[');
                }
                ']' => {
                    regex.push(']');
                }
                '\\' | '/' => {
                    // 路径分隔符标准化为正则表达式
                    regex.push_str(r"[/\\]");
                }
                c if "^$(){}|+.".contains(c) => {
                    // 转义正则表达式特殊字符
                    regex.push('\\');
                    regex.push(c);
                }
                c => {
                    regex.push(c);
                }
            }
            i += 1;
        }

        regex.push('$');
        Ok(regex)
    }

    /// 应用排除模式到文件列表
    fn apply_exclude_patterns(&self, files: Vec<PathBuf>, exclude_patterns: &[&str]) -> Result<Vec<PathBuf>> {
        if exclude_patterns.is_empty() {
            return Ok(files);
        }

        let original_count = files.len();
        let mut filtered_files = Vec::new();

        for file_path in files {
            let mut should_exclude = false;
            
            for pattern in exclude_patterns {
                if self.matches_pattern(&file_path, pattern)? {
                    should_exclude = true;
                    break;
                }
            }
            
            if !should_exclude {
                filtered_files.push(file_path);
            }
        }

        if original_count != filtered_files.len() {
            println!("Excluded {} files based on exclude patterns", original_count - filtered_files.len());
        }

        Ok(filtered_files)
    }

    /// 应用排除模式到已存储的文件列表
    fn apply_exclude_patterns_to_stored(&self, files: Vec<PathBuf>, exclude_patterns: &[&str]) -> Result<Vec<PathBuf>> {
        if exclude_patterns.is_empty() {
            return Ok(files);
        }

        let original_count = files.len();
        let mut filtered_files = Vec::new();

        for file_path in files {
            let mut should_exclude = false;
            
            for pattern in exclude_patterns {
                // 将通配符模式转换为正则表达式进行匹配
                let regex_pattern = self.glob_to_regex(pattern)?;
                let regex = regex::Regex::new(&regex_pattern)
                    .context("Failed to compile exclude regex pattern")?;
                    
                let path_str = file_path.to_string_lossy();
                if regex.is_match(&path_str) {
                    should_exclude = true;
                    break;
                }
            }
            
            if !should_exclude {
                filtered_files.push(file_path);
            }
        }

        if original_count != filtered_files.len() {
            println!("Excluded {} stored files based on exclude patterns", original_count - filtered_files.len());
        }

        Ok(filtered_files)
    }

    /// 检查文件路径是否匹配通配符模式
    fn matches_pattern(&self, file_path: &Path, pattern: &str) -> Result<bool> {
        // 使用glob进行文件系统匹配
        for entry in glob(pattern).context("Failed to parse glob pattern")? {
            match entry {
                Ok(path) => {
                    if path == file_path {
                        return Ok(true);
                    }
                }
                Err(_) => continue,
            }
        }
        Ok(false)
    }

    pub fn owe_all_files(&mut self) -> Result<()> {
        let files = self.index.list_files()?;
        
        if files.is_empty() {
            println!("No files stored.");
            return Ok(());
        }

        println!("Extracting {} stored files...", files.len());
        
        for entry in files {
            match self.owe_file(&entry.original_path) {
                Ok(()) => {
                    println!("✓ Extracted: {}", entry.original_path.display());
                }
                Err(e) => {
                    eprintln!("✗ Failed to extract {}: {}", entry.original_path.display(), e);
                }
            }
        }

        println!("Extraction complete.");
        Ok(())
    }

    // 多线程存储文件
    fn store_files_parallel(&mut self, files: Vec<PathBuf>, delete_source: bool) -> Result<()> {
        use rayon::prelude::*;
        
        // 设置全局线程池
        rayon::ThreadPoolBuilder::new()
            .num_threads(self.config.multithread)
            .build_global()
            .unwrap_or_else(|_| {
                // 如果全局线程池已存在，继续使用
            });

        let config = self.config.clone();
        
        // 并行处理文件
        let results: Vec<Result<FileEntry>> = files
            .par_iter()
            .map(|file_path| {
                Self::process_single_file_static(file_path, delete_source, &config)
            })
            .collect();

        // 批量添加到索引
        let mut success_count = 0;
        for result in results {
            match result {
                Ok(entry) => {
                    self.index.add_file(entry)?;
                    success_count += 1;
                }
                Err(e) => {
                    eprintln!("Failed to store file: {}", e);
                }
            }
        }

        println!("Stored {} files using {} threads", success_count, self.config.multithread);
        Ok(())
    }

    // 静态方法处理单个文件存储（用于多线程）
    fn process_single_file_static(file_path: &Path, delete_source: bool, config: &Config) -> Result<FileEntry> {
        if !file_path.exists() {
            return Err(anyhow::anyhow!("File does not exist: {}", file_path.display()));
        }

        if !file_path.is_file() {
            return Err(anyhow::anyhow!("Path is not a file: {}", file_path.display()));
        }

        // 生成唯一ID和存储路径
        let id = Uuid::new_v4().to_string();
        let stored_filename = format!("{}.gz", id);
        let stored_path = config.storage_path.join(&stored_filename);

        // 确保存储目录存在
        fs::create_dir_all(&config.storage_path)
            .context("Failed to create storage directory")?;

        // 获取原始文件大小
        let file_size = fs::metadata(file_path)?.len();

        // 压缩并存储文件
        let compressed_size = Self::compress_file_static(file_path, &stored_path, config)
            .context("Failed to compress file")?;

        // 如果需要删除源文件
        if delete_source {
            fs::remove_file(file_path)
                .context("Failed to delete source file")?;
        }

        // 创建索引条目
        let entry = FileEntry {
            id,
            original_path: file_path.to_path_buf(),
            stored_path,
            file_size,
            compressed_size,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        println!("File stored successfully: {} (compression: {:.1}%)", 
                 file_path.display(),
                 (compressed_size as f64 / file_size as f64) * 100.0);

        Ok(entry)
    }

    // 静态压缩文件方法
    fn compress_file_static(input_path: &Path, output_path: &Path, config: &Config) -> Result<u64> {
        let mut input_file = File::open(input_path)
            .context("Failed to open input file")?;
        let output_file = File::create(output_path)
            .context("Failed to create output file")?;

        // 使用配置中设置的压缩级别
        let compression_level = Compression::new(config.compression_level);

        let mut encoder = GzEncoder::new(output_file, compression_level);
        io::copy(&mut input_file, &mut encoder)
            .context("Failed to compress file")?;

        encoder.finish()
            .context("Failed to finalize compression")?;

        let compressed_size = fs::metadata(output_path)?.len();
        Ok(compressed_size)
    }

    // 多线程提取文件
    fn owe_files_parallel(&mut self, files: Vec<PathBuf>) -> Result<()> {
        use rayon::prelude::*;
          // 设置全局线程池
        rayon::ThreadPoolBuilder::new()
            .num_threads(self.config.multithread)
            .build_global()
            .unwrap_or_else(|_| {
                // 如果全局线程池已存在，继续使用
            });

        // 先获取所有文件的索引条目
        let mut entries = Vec::new();
        for file_path in &files {
            if let Some(entry) = self.index.get_file(file_path)? {
                entries.push(entry);
            }
        }

        // 并行处理文件解压
        let results: Vec<Result<PathBuf>> = entries
            .par_iter()
            .map(|entry| {
                Self::decompress_file_static(&entry.stored_path, &entry.original_path)
                    .map(|_| entry.original_path.clone())
            })
            .collect();

        // 批量处理结果
        let mut success_count = 0;
        for (i, result) in results.into_iter().enumerate() {
            match result {
                Ok(file_path) => {
                    // 删除压缩的存储文件
                    if let Err(e) = fs::remove_file(&entries[i].stored_path) {
                        eprintln!("Failed to remove stored file {}: {}", entries[i].stored_path.display(), e);
                    }
                    
                    // 从索引中移除
                    if let Err(e) = self.index.remove_file(&file_path) {
                        eprintln!("Failed to remove from index {}: {}", file_path.display(), e);
                    } else {
                        success_count += 1;
                        println!("File extracted successfully: {}", file_path.display());
                    }
                }
                Err(e) => {
                    eprintln!("Failed to extract file: {}", e);
                }
            }
        }

        println!("Extracted {} files using {} threads", success_count, self.config.multithread);
        Ok(())
    }

    // 静态解压文件方法
    fn decompress_file_static(input_path: &Path, output_path: &Path) -> Result<()> {
        let input_file = File::open(input_path)
            .context("Failed to open compressed file")?;
        let mut decoder = GzDecoder::new(input_file);

        // 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create output directory")?;
        }

        let mut output_file = File::create(output_path)
            .context("Failed to create output file")?;

        io::copy(&mut decoder, &mut output_file)
            .context("Failed to decompress file")?;

        Ok(())
    }
}
