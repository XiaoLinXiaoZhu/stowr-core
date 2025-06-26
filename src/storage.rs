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
use crate::dedup::ContentDeduplicator;
use crate::delta::DeltaStorage;

pub struct StorageManager {
    config: Config,
    index: Box<dyn IndexStore>,
    deduplicator: ContentDeduplicator,
    delta_storage: DeltaStorage,
}

impl StorageManager {
    pub fn new(config: Config, index: Box<dyn IndexStore>) -> Self {
        let deduplicator = ContentDeduplicator::new();
        let delta_storage = DeltaStorage::new(
            config.similarity_threshold,
            config.delta_algorithm.clone(),
        );

        let mut manager = Self {
            config,
            index,
            deduplicator,
            delta_storage,
        };

        // 从现有索引重建去重器状态
        if let Err(e) = manager.rebuild_dedup_state() {
            eprintln!("Warning: Failed to rebuild deduplication state: {}", e);
        }

        manager
    }

    pub fn store_file(&mut self, file_path: &Path, delete_source: bool) -> Result<()> {
        if !file_path.exists() {
            return Err(anyhow::anyhow!("File does not exist: {}", file_path.display()));
        }

        if !file_path.is_file() {
            return Err(anyhow::anyhow!("Path is not a file: {}", file_path.display()));
        }

        // 检查文件路径是否已经存储（防止重复存储同一路径）
        if self.index.get_file(file_path)?.is_some() {
            println!("File already stored: {}", file_path.display());
            if delete_source {
                fs::remove_file(file_path)
                    .context("Failed to delete source file")?;
                println!("Source file deleted: {}", file_path.display());
            }
            return Ok(());
        }

        // 计算文件哈希进行内容去重
        let file_content = fs::read(file_path)
            .context("Failed to read file for hashing")?;
        let file_hash = ContentDeduplicator::calculate_hash(&file_content);

        // 检查是否启用去重功能
        if self.config.enable_deduplication {
            if let Some(existing_entry) = self.find_file_by_hash(&file_hash)? {
                // 文件内容完全相同，创建引用
                let entry = self.create_reference_entry(file_path, &existing_entry)?;
                self.index.add_file(entry)?;
                
                // 增加去重器中的引用计数
                self.deduplicator.add_hash_reference(&file_hash, &existing_entry.id);
                
                if delete_source {
                    fs::remove_file(file_path)
                        .context("Failed to delete source file")?;
                    println!("Source file deleted: {}", file_path.display());
                }
                
                println!("File deduplicated (reference created): {}", file_path.display());
                println!("References existing file with hash: {}", file_hash);
                return Ok(());
            }
        }

        // 检查是否启用差分存储
        if self.config.enable_delta_compression {
            if let Some((base_entry, similarity)) = self.find_similar_file(&file_content)? {
                if similarity >= self.config.similarity_threshold {
                    // 创建差分文件
                    return self.store_as_delta(file_path, &file_content, &base_entry, similarity, delete_source);
                }
            }
        }

        // 作为新的基础文件存储
        self.store_as_base_file(file_path, &file_content, file_hash, delete_source)
    }

    pub fn owe_file(&mut self, file_path: &Path) -> Result<()> {
        let entry = self.index.get_file(file_path)?
            .ok_or_else(|| anyhow::anyhow!("File not found in storage: {}", file_path.display()))?;

        // 根据文件类型处理不同的提取逻辑
        if entry.is_reference.unwrap_or(false) {
            // 引用文件：从原始存储位置提取内容
            self.extract_reference_file(&entry)?;
        } else if entry.is_delta.unwrap_or(false) {
            // 差分文件：重建原文件
            self.extract_delta_file(&entry)?;
        } else {
            // 基础文件：直接解压缩
            self.decompress_file(&entry.stored_path, &entry.original_path)
                .context("Failed to decompress file")?;
            
            // 对于基础文件，也需要处理引用计数
            let should_delete_from_dedup = if let Some(hash) = &entry.hash {
                self.deduplicator.remove_hash_reference(hash)
            } else {
                true // 如果没有哈希值，说明不是去重文件，可以删除
            };
            
            // 检查是否还有其他引用
            let has_references = self.has_references_to_storage(&entry.id)?;
            
            // 只有当去重器认为可以删除且没有其他引用时才删除存储文件
            if should_delete_from_dedup && !has_references && entry.stored_path.exists() {
                fs::remove_file(&entry.stored_path)
                    .context("Failed to remove stored file")?;
            }
        }

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

    fn decompress_file(&self, input_path: &Path, output_path: &Path) -> Result<()> {
        // 根据文件扩展名确定压缩算法
        let algorithm = if let Some(ext) = input_path.extension() {
            match ext.to_str() {
                Some("gz") => crate::config::CompressionAlgorithm::Gzip,
                Some("zst") => crate::config::CompressionAlgorithm::Zstd,
                Some("lz4") => crate::config::CompressionAlgorithm::Lz4,
                _ => return Err(anyhow::anyhow!("Unsupported file extension: {:?}", ext)),
            }
        } else {
            return Err(anyhow::anyhow!("No file extension found"));
        };

        match algorithm {
            crate::config::CompressionAlgorithm::Gzip => {
                self.decompress_file_gzip(input_path, output_path)
            }
            crate::config::CompressionAlgorithm::Zstd => {
                self.decompress_file_zstd(input_path, output_path)
            }
            crate::config::CompressionAlgorithm::Lz4 => {
                self.decompress_file_lz4(input_path, output_path)
            }
        }
    }

    fn decompress_file_gzip(&self, input_path: &Path, output_path: &Path) -> Result<()> {
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

    fn decompress_file_zstd(&self, input_path: &Path, output_path: &Path) -> Result<()> {
        let compressed_data = fs::read(input_path)
            .context("Failed to read compressed file")?;

        let decompressed_data = zstd::decode_all(compressed_data.as_slice())
            .context("Failed to decompress with zstd")?;

        // 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create output directory")?;
        }

        fs::write(output_path, decompressed_data)
            .context("Failed to write decompressed file")?;

        Ok(())
    }

    fn decompress_file_lz4(&self, input_path: &Path, output_path: &Path) -> Result<()> {
        let compressed_data = fs::read(input_path)
            .context("Failed to read compressed file")?;

        let decompressed_data = lz4_flex::decompress_size_prepended(&compressed_data)
            .context("Failed to decompress with lz4")?;

        // 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create output directory")?;
        }

        fs::write(output_path, decompressed_data)
            .context("Failed to write decompressed file")?;

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
        // 对于去重和差分存储，我们需要顺序处理以正确比较文件
        // 多线程会破坏去重和差分存储的逻辑，因为需要访问共享的索引和去重器状态
        println!("Processing {} files sequentially to enable deduplication and delta compression...", files.len());
        
        let mut success_count = 0;
        for file_path in files {
            match self.store_file(&file_path, delete_source) {
                Ok(()) => {
                    success_count += 1;
                }
                Err(e) => {
                    eprintln!("Failed to store {}: {}", file_path.display(), e);
                }
            }
        }

        println!("Stored {} files with deduplication and delta compression enabled", success_count);
        Ok(())
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
        // 根据文件扩展名确定压缩算法
        let algorithm = if let Some(ext) = input_path.extension() {
            match ext.to_str() {
                Some("gz") => crate::config::CompressionAlgorithm::Gzip,
                Some("zst") => crate::config::CompressionAlgorithm::Zstd,
                Some("lz4") => crate::config::CompressionAlgorithm::Lz4,
                _ => return Err(anyhow::anyhow!("Unsupported file extension: {:?}", ext)),
            }
        } else {
            return Err(anyhow::anyhow!("No file extension found"));
        };

        match algorithm {
            crate::config::CompressionAlgorithm::Gzip => {
                Self::decompress_file_gzip_static(input_path, output_path)
            }
            crate::config::CompressionAlgorithm::Zstd => {
                Self::decompress_file_zstd_static(input_path, output_path)
            }
            crate::config::CompressionAlgorithm::Lz4 => {
                Self::decompress_file_lz4_static(input_path, output_path)
            }
        }
    }

    fn decompress_file_gzip_static(input_path: &Path, output_path: &Path) -> Result<()> {
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

    fn decompress_file_zstd_static(input_path: &Path, output_path: &Path) -> Result<()> {
        let compressed_data = fs::read(input_path)
            .context("Failed to read compressed file")?;

        let decompressed_data = zstd::decode_all(compressed_data.as_slice())
            .context("Failed to decompress with zstd")?;

        // 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create output directory")?;
        }

        fs::write(output_path, decompressed_data)
            .context("Failed to write decompressed file")?;

        Ok(())
    }

    fn decompress_file_lz4_static(input_path: &Path, output_path: &Path) -> Result<()> {
        let compressed_data = fs::read(input_path)
            .context("Failed to read compressed file")?;

        let decompressed_data = lz4_flex::decompress_size_prepended(&compressed_data)
            .context("Failed to decompress with lz4")?;

        // 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create output directory")?;
        }

        fs::write(output_path, decompressed_data)
            .context("Failed to write decompressed file")?;

        Ok(())
    }

    /// 获取去重统计信息
    pub fn get_dedup_stats(&self) -> crate::dedup::DedupStats {
        self.deduplicator.get_stats()
    }

    /// 获取差分存储统计信息
    pub fn get_delta_stats(&self) -> crate::delta::DeltaStats {
        self.delta_storage.get_stats()
    }

    /// 检查是否启用去重功能
    pub fn is_dedup_enabled(&self) -> bool {
        self.config.enable_deduplication
    }

    /// 检查是否启用差分存储功能
    pub fn is_delta_enabled(&self) -> bool {
        self.config.enable_delta_compression
    }

    /// 获取当前相似度阈值
    pub fn get_similarity_threshold(&self) -> f32 {
        self.config.similarity_threshold
    }

    /// 根据哈希值查找基础文件（用于去重）
    fn find_file_by_hash(&self, hash: &str) -> Result<Option<FileEntry>> {
        let all_files = self.index.list_files()?;
        for file in all_files {
            if let Some(file_hash) = &file.hash {
                if file_hash == hash {
                    // 只返回基础文件（非引用、非差分文件）
                    if !file.is_reference.unwrap_or(false) && !file.is_delta.unwrap_or(false) {
                        return Ok(Some(file));
                    }
                }
            }
        }
        Ok(None)
    }

    /// 查找相似文件用于差分存储
    fn find_similar_file(&self, content: &[u8]) -> Result<Option<(FileEntry, f32)>> {
        let all_files = self.index.list_files()?;
        let mut best_match: Option<(FileEntry, f32)> = None;

        for file in all_files {
            // 只考虑基础文件（非引用、非差分文件）
            if file.is_reference.unwrap_or(false) || file.is_delta.unwrap_or(false) {
                continue;
            }

            // 读取已存储的文件内容进行比较
            if let Ok(stored_content) = self.read_stored_file_content(&file) {
                let similarity = self.delta_storage.calculate_similarity(content, &stored_content);
                
                if let Some((_, current_best)) = &best_match {
                    if similarity > *current_best {
                        best_match = Some((file, similarity));
                    }
                } else {
                    best_match = Some((file, similarity));
                }
            }
        }

        Ok(best_match)
    }

    /// 读取已存储文件的内容
    fn read_stored_file_content(&self, entry: &FileEntry) -> Result<Vec<u8>> {
        // 先解压缩文件到临时位置，然后读取内容
        let compressed_data = fs::read(&entry.stored_path)
            .context("Failed to read stored file")?;

        match entry.compression_algorithm {
            crate::config::CompressionAlgorithm::Gzip => {
                let mut decoder = GzDecoder::new(compressed_data.as_slice());
                let mut content = Vec::new();
                std::io::Read::read_to_end(&mut decoder, &mut content)
                    .context("Failed to decompress gzip file")?;
                Ok(content)
            }
            crate::config::CompressionAlgorithm::Zstd => {
                zstd::decode_all(compressed_data.as_slice())
                    .context("Failed to decompress zstd file")
            }
            crate::config::CompressionAlgorithm::Lz4 => {
                lz4_flex::decompress_size_prepended(&compressed_data)
                    .context("Failed to decompress lz4 file")
            }
        }
    }

    /// 创建引用条目（用于去重）
    fn create_reference_entry(&self, file_path: &Path, existing_entry: &FileEntry) -> Result<FileEntry> {
        let id = Uuid::new_v4().to_string();
        let mut entry = FileEntry::new(
            id,
            file_path.to_path_buf(),
            existing_entry.stored_path.clone(), // 引用同样的存储路径
            existing_entry.file_size,
            0, // 引用文件的压缩大小为0
            existing_entry.compression_algorithm.clone(),
        );

        // 设置引用相关字段
        entry.is_reference = Some(true);
        entry.base_storage_id = Some(existing_entry.id.clone());
        entry.hash = existing_entry.hash.clone();

        Ok(entry)
    }

    /// 存储为差分文件
    fn store_as_delta(
        &mut self,
        file_path: &Path,
        content: &[u8],
        base_entry: &FileEntry,
        similarity: f32,
        delete_source: bool,
    ) -> Result<()> {
        // 读取基础文件内容
        let base_content = self.read_stored_file_content(base_entry)?;

        // 创建差分数据
        let delta_data = self.delta_storage.create_delta(&base_content, content)?;

        // 生成存储ID和路径
        let id = Uuid::new_v4().to_string();
        let extension = self.config.compression_algorithm.file_extension();
        let stored_filename = format!("{}.{}", id, extension);
        let stored_path = self.config.storage_path.join(&stored_filename);

        // 确保存储目录存在
        fs::create_dir_all(&self.config.storage_path)
            .context("Failed to create storage directory")?;

        // 压缩并存储差分数据
        let compressed_size = self.compress_data(&delta_data, &stored_path)
            .context("Failed to compress delta data")?;

        // 创建索引条目
        let mut entry = FileEntry::new(
            id,
            file_path.to_path_buf(),
            stored_path,
            content.len() as u64,
            compressed_size,
            self.config.compression_algorithm.clone(),
        );

        // 设置差分相关字段
        entry.is_delta = Some(true);
        entry.base_storage_id = Some(base_entry.id.clone());
        entry.similarity_score = Some(similarity);
        entry.hash = Some(ContentDeduplicator::calculate_hash(content));

        // 添加到索引
        self.index.add_file(entry)
            .context("Failed to add delta file to index")?;

        // 删除源文件（如果需要）
        if delete_source {
            fs::remove_file(file_path)
                .context("Failed to delete source file")?;
            println!("Source file deleted: {}", file_path.display());
        }

        println!("File stored as delta: {}", file_path.display());
        println!("Similarity: {:.1}%, Delta size: {:.1}%", 
                 similarity * 100.0,
                 (compressed_size as f64 / content.len() as f64) * 100.0);

        Ok(())
    }

    /// 存储为基础文件
    fn store_as_base_file(
        &mut self,
        file_path: &Path,
        content: &[u8],
        hash: String,
        delete_source: bool,
    ) -> Result<()> {
        // 生成唯一ID和存储路径
        let id = Uuid::new_v4().to_string();
        let extension = self.config.compression_algorithm.file_extension();
        let stored_filename = format!("{}.{}", id, extension);
        let stored_path = self.config.storage_path.join(&stored_filename);

        // 确保存储目录存在
        fs::create_dir_all(&self.config.storage_path)
            .context("Failed to create storage directory")?;

        // 压缩并存储文件
        let compressed_size = self.compress_data(content, &stored_path)
            .context("Failed to compress file")?;

        // 创建索引条目
        let mut entry = FileEntry::new(
            id.clone(),
            file_path.to_path_buf(),
            stored_path,
            content.len() as u64,
            compressed_size,
            self.config.compression_algorithm.clone(),
        );

        // 设置哈希值
        entry.hash = Some(hash.clone());

        // 注册到去重器（如果启用）
        if self.config.enable_deduplication {
            self.deduplicator.register_file(hash, id);
        }

        // 添加到索引
        self.index.add_file(entry)
            .context("Failed to add file to index")?;

        // 删除源文件（如果需要）
        if delete_source {
            fs::remove_file(file_path)
                .context("Failed to delete source file")?;
            println!("Source file deleted: {}", file_path.display());
        }

        println!("File stored successfully: {}", file_path.display());
        println!("Compression ratio: {:.1}%", 
                 (compressed_size as f64 / content.len() as f64) * 100.0);

        Ok(())
    }

    /// 压缩数据到指定路径
    fn compress_data(&self, data: &[u8], output_path: &Path) -> Result<u64> {
        match self.config.compression_algorithm {
            crate::config::CompressionAlgorithm::Gzip => {
                let output_file = File::create(output_path)
                    .context("Failed to create output file")?;
                let mut encoder = GzEncoder::new(output_file, Compression::new(self.config.compression_level as u32));
                std::io::Write::write_all(&mut encoder, data)
                    .context("Failed to write compressed data")?;
                encoder.finish()
                    .context("Failed to finish compression")?;
                
                Ok(fs::metadata(output_path)?.len())
            }
            crate::config::CompressionAlgorithm::Zstd => {
                let compressed_data = zstd::encode_all(data, self.config.compression_level as i32)
                    .context("Failed to compress with zstd")?;
                fs::write(output_path, &compressed_data)
                    .context("Failed to write compressed file")?;
                
                Ok(compressed_data.len() as u64)
            }
            crate::config::CompressionAlgorithm::Lz4 => {
                let compressed_data = lz4_flex::compress_prepend_size(data);
                fs::write(output_path, &compressed_data)
                    .context("Failed to write compressed file")?;
                
                Ok(compressed_data.len() as u64)
            }
        }
    }

    /// 提取引用文件
    fn extract_reference_file(&mut self, entry: &FileEntry) -> Result<()> {
        // 引用文件的stored_path指向原始存储文件
        // 直接解压缩到目标位置
        self.decompress_file(&entry.stored_path, &entry.original_path)
            .context("Failed to decompress reference file")?;

        // 对于引用文件，检查是否需要删除基础存储文件
        if let Some(base_storage_id) = &entry.base_storage_id {
            // 检查是否有其他文件（除了当前文件）仍在引用这个存储
            let has_other_references = self.has_other_references_to_storage(base_storage_id, &entry.original_path)?;
            
            // 如果当前文件有哈希值，更新去重器的引用计数
            let should_delete_from_dedup = if let Some(hash) = &entry.hash {
                self.deduplicator.remove_hash_reference(hash)
            } else {
                false
            };
            
            // 只有当没有其他引用且去重器也认为应该删除时才删除物理文件
            if !has_other_references && should_delete_from_dedup && entry.stored_path.exists() {
                fs::remove_file(&entry.stored_path)
                    .context("Failed to remove stored file")?;
            }
        }

        Ok(())
    }

    /// 提取差分文件
    fn extract_delta_file(&mut self, entry: &FileEntry) -> Result<()> {
        // 获取基础文件ID
        let base_storage_id = entry.base_storage_id.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Delta file missing base storage ID"))?;

        // 查找基础文件
        let base_entry = self.find_file_by_storage_id(base_storage_id)?
            .ok_or_else(|| anyhow::anyhow!("Base file not found for delta: {}", base_storage_id))?;

        // 读取基础文件内容
        let base_content = self.read_stored_file_content(&base_entry)?;

        // 读取差分数据
        let delta_data = self.read_stored_file_content(entry)?;

        // 应用差分重建原文件
        let reconstructed_content = self.delta_storage.apply_delta(&base_content, &delta_data)?;

        // 确保输出目录存在
        if let Some(parent) = entry.original_path.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create output directory")?;
        }

        // 写入重建的文件
        fs::write(&entry.original_path, reconstructed_content)
            .context("Failed to write reconstructed file")?;

        // 删除差分存储文件
        if entry.stored_path.exists() {
            fs::remove_file(&entry.stored_path)
                .context("Failed to remove delta file")?;
        }

        Ok(())
    }

    /// 根据存储ID查找文件
    fn find_file_by_storage_id(&self, storage_id: &str) -> Result<Option<FileEntry>> {
        let all_files = self.index.list_files()?;
        for file in all_files {
            if file.id == storage_id {
                return Ok(Some(file));
            }
        }
        Ok(None)
    }

    /// 从现有索引重建去重器状态
    fn rebuild_dedup_state(&mut self) -> Result<()> {
        let all_files = self.index.list_files()?;
        let mut dedup_entries = Vec::new();

        for file in all_files {
            if let Some(hash) = &file.hash {
                // 只有基础文件（非引用、非差分）才需要注册到去重器
                if !file.is_reference.unwrap_or(false) && !file.is_delta.unwrap_or(false) {
                    // 计算引用计数（包括自己）
                    let ref_count = self.count_references_for_hash(hash)?;
                    dedup_entries.push((file.id.clone(), hash.clone(), ref_count));
                }
            }
        }

        self.deduplicator.rebuild_from_index(dedup_entries)?;
        Ok(())
    }

    /// 计算特定哈希值的引用计数
    fn count_references_for_hash(&self, target_hash: &str) -> Result<u32> {
        let all_files = self.index.list_files()?;
        let mut count = 0;

        for file in all_files {
            if let Some(hash) = &file.hash {
                if hash == target_hash {
                    count += 1;
                }
            }
        }

        Ok(count)
    }

    /// 检查是否有其他文件引用指定的存储ID
    fn has_references_to_storage(&self, storage_id: &str) -> Result<bool> {
        let all_files = self.index.list_files()?;
        
        for file in all_files {
            // 检查引用文件
            if file.is_reference.unwrap_or(false) {
                if let Some(base_id) = &file.base_storage_id {
                    if base_id == storage_id {
                        return Ok(true);
                    }
                }
            }
            
            // 检查差分文件
            if file.is_delta.unwrap_or(false) {
                if let Some(base_id) = &file.base_storage_id {
                    if base_id == storage_id {
                        return Ok(true);
                    }
                }
            }
        }
        
        Ok(false)
    }

    /// 检查是否有其他文件（除了指定文件）引用指定的存储ID
    fn has_other_references_to_storage(&self, storage_id: &str, exclude_path: &Path) -> Result<bool> {
        let all_files = self.index.list_files()?;
        
        for file in all_files {
            // 跳过指定要排除的文件
            if file.original_path == exclude_path {
                continue;
            }
            
            // 检查引用文件
            if file.is_reference.unwrap_or(false) {
                if let Some(base_id) = &file.base_storage_id {
                    if base_id == storage_id {
                        return Ok(true);
                    }
                }
            }
            
            // 检查差分文件
            if file.is_delta.unwrap_or(false) {
                if let Some(base_id) = &file.base_storage_id {
                    if base_id == storage_id {
                        return Ok(true);
                    }
                }
            }
        }
        
        Ok(false)
    }
}
