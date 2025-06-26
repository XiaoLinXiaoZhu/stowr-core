use std::collections::HashMap;
use sha2::{Sha256, Digest};
use serde::{Deserialize, Serialize};
use anyhow::Result;

/// 内容去重器
/// 
/// 通过计算文件的SHA256哈希值来识别完全相同的文件，
/// 实现内容级别的去重存储。
#[derive(Debug)]
pub struct ContentDeduplicator {
    /// 哈希值到存储ID的映射
    hash_to_storage: HashMap<String, String>,
    /// 存储ID到引用计数的映射
    ref_counts: HashMap<String, u32>,
    /// 存储ID到哈希值的反向映射
    storage_to_hash: HashMap<String, String>,
}

/// 去重存储信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DedupInfo {
    /// 是否为引用（重复文件）
    pub is_reference: bool,
    /// 原始存储ID（对于引用文件指向的原文件ID）
    pub original_storage_id: Option<String>,
    /// 文件哈希值
    pub hash: String,
    /// 引用计数
    pub ref_count: u32,
}

impl ContentDeduplicator {
    /// 创建新的内容去重器
    pub fn new() -> Self {
        Self {
            hash_to_storage: HashMap::new(),
            ref_counts: HashMap::new(),
            storage_to_hash: HashMap::new(),
        }
    }

    /// 计算数据的SHA256哈希值
    pub fn calculate_hash(data: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data);
        format!("{:x}", hasher.finalize())
    }

    /// 检查文件是否重复
    /// 
    /// 返回 Some(storage_id) 如果文件已存在，None 如果是新文件
    pub fn check_duplicate(&mut self, hash: &str) -> Option<String> {
        if let Some(storage_id) = self.hash_to_storage.get(hash) {
            // 增加引用计数
            *self.ref_counts.entry(storage_id.clone()).or_insert(0) += 1;
            Some(storage_id.clone())
        } else {
            None
        }
    }

    /// 注册新文件
    /// 
    /// 当存储新文件时调用，建立哈希值和存储ID的映射
    pub fn register_file(&mut self, hash: String, storage_id: String) {
        self.hash_to_storage.insert(hash.clone(), storage_id.clone());
        self.storage_to_hash.insert(storage_id.clone(), hash);
        self.ref_counts.insert(storage_id, 1);
    }

    /// 移除文件引用
    /// 
    /// 减少引用计数，如果计数为0则完全移除
    /// 返回是否应该删除物理文件
    pub fn remove_reference(&mut self, storage_id: &str) -> bool {
        if let Some(count) = self.ref_counts.get_mut(storage_id) {
            *count -= 1;
            if *count == 0 {
                // 引用计数为0，清理所有相关映射
                self.ref_counts.remove(storage_id);
                if let Some(hash) = self.storage_to_hash.remove(storage_id) {
                    self.hash_to_storage.remove(&hash);
                }
                true // 应该删除物理文件
            } else {
                false // 还有其他引用，不删除物理文件
            }
        } else {
            true // 如果找不到引用记录，默认删除
        }
    }

    /// 通过哈希值移除引用
    /// 
    /// 减少对应存储的引用计数，如果计数为0则完全移除
    /// 返回是否应该删除物理文件
    pub fn remove_hash_reference(&mut self, hash: &str) -> bool {
        if let Some(storage_id) = self.hash_to_storage.get(hash) {
            let storage_id = storage_id.clone(); // 避免借用冲突
            self.remove_reference(&storage_id)
        } else {
            true // 如果找不到对应的存储，默认删除
        }
    }

    /// 通过哈希值增加引用
    /// 
    /// 增加对应存储的引用计数
    pub fn add_hash_reference(&mut self, hash: &str, storage_id: &str) {
        if let Some(existing_storage_id) = self.hash_to_storage.get(hash) {
            // 验证存储ID是否匹配
            if existing_storage_id == storage_id {
                // 增加引用计数
                *self.ref_counts.entry(storage_id.to_string()).or_insert(0) += 1;
            }
        } else {
            // 如果哈希不存在，这可能是一个错误状态，但我们可以尝试修复
            self.hash_to_storage.insert(hash.to_string(), storage_id.to_string());
            self.storage_to_hash.insert(storage_id.to_string(), hash.to_string());
            *self.ref_counts.entry(storage_id.to_string()).or_insert(0) += 1;
        }
    }

    /// 获取文件的去重信息
    pub fn get_dedup_info(&self, storage_id: &str) -> Option<DedupInfo> {
        if let Some(hash) = self.storage_to_hash.get(storage_id) {
            let ref_count = self.ref_counts.get(storage_id).copied().unwrap_or(0);
            Some(DedupInfo {
                is_reference: ref_count > 1,
                original_storage_id: None, // 对于原文件，这个字段为None
                hash: hash.clone(),
                ref_count,
            })
        } else {
            None
        }
    }

    /// 获取引用信息（用于引用文件）
    pub fn get_reference_info(&self, hash: &str) -> Option<DedupInfo> {
        if let Some(storage_id) = self.hash_to_storage.get(hash) {
            let ref_count = self.ref_counts.get(storage_id).copied().unwrap_or(0);
            Some(DedupInfo {
                is_reference: true,
                original_storage_id: Some(storage_id.clone()),
                hash: hash.to_string(),
                ref_count,
            })
        } else {
            None
        }
    }

    /// 获取所有存储的统计信息
    pub fn get_stats(&self) -> DedupStats {
        let total_files = self.ref_counts.values().sum::<u32>();
        let unique_files = self.ref_counts.len() as u32;
        let duplicate_files = total_files.saturating_sub(unique_files);
        
        DedupStats {
            total_files,
            unique_files,
            duplicate_files,
            dedup_ratio: if total_files > 0 {
                duplicate_files as f32 / total_files as f32
            } else {
                0.0
            },
        }
    }

    /// 从索引数据重建去重器状态
    pub fn rebuild_from_index(&mut self, entries: Vec<(String, String, u32)>) -> Result<()> {
        // entries: (storage_id, hash, ref_count)
        self.hash_to_storage.clear();
        self.ref_counts.clear();
        self.storage_to_hash.clear();

        for (storage_id, hash, ref_count) in entries {
            self.hash_to_storage.insert(hash.clone(), storage_id.clone());
            self.storage_to_hash.insert(storage_id.clone(), hash);
            self.ref_counts.insert(storage_id, ref_count);
        }

        Ok(())
    }
}

impl Default for ContentDeduplicator {
    fn default() -> Self {
        Self::new()
    }
}

/// 去重统计信息
#[derive(Debug, Clone)]
pub struct DedupStats {
    /// 总文件数（包括重复）
    pub total_files: u32,
    /// 唯一文件数
    pub unique_files: u32,
    /// 重复文件数
    pub duplicate_files: u32,
    /// 去重率（重复文件数/总文件数）
    pub dedup_ratio: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deduplicator_basic() {
        let mut dedup = ContentDeduplicator::new();
        
        // 测试新文件
        let hash1 = "abc123".to_string();
        assert_eq!(dedup.check_duplicate(&hash1), None);
        
        // 注册文件
        dedup.register_file(hash1.clone(), "storage1".to_string());
        
        // 测试重复文件
        assert_eq!(dedup.check_duplicate(&hash1), Some("storage1".to_string()));
        
        // 测试引用计数
        let info = dedup.get_dedup_info("storage1").unwrap();
        assert_eq!(info.ref_count, 2); // 1 original + 1 reference
    }

    #[test]
    fn test_hash_calculation() {
        let data = b"Hello, World!";
        let hash = ContentDeduplicator::calculate_hash(data);
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 64); // SHA256 produces 64-character hex string
    }

    #[test]
    fn test_remove_reference() {
        let mut dedup = ContentDeduplicator::new();
        
        dedup.register_file("hash1".to_string(), "storage1".to_string());
        dedup.check_duplicate("hash1"); // 增加一个引用
        
        // 移除一个引用，应该不删除文件
        assert!(!dedup.remove_reference("storage1"));
        
        // 移除最后一个引用，应该删除文件
        assert!(dedup.remove_reference("storage1"));
    }

    #[test]
    fn test_remove_reference_by_hash() {
        let mut dedup = ContentDeduplicator::new();
        
        dedup.register_file("hash1".to_string(), "storage1".to_string());
        dedup.check_duplicate("hash1"); // 增加一个引用
        
        // 通过哈希值移除引用，应该不删除文件
        assert!(!dedup.remove_hash_reference("hash1"));
        
        // 通过哈希值移除最后一个引用，应该删除文件
        assert!(dedup.remove_hash_reference("hash1"));
    }

    #[test]
    fn test_add_reference_by_hash() {
        let mut dedup = ContentDeduplicator::new();
        
        dedup.register_file("hash1".to_string(), "storage1".to_string());
        
        // 通过哈希值增加引用
        dedup.add_hash_reference("hash1", "storage1");
        
        // 引用计数应该增加
        let info = dedup.get_dedup_info("storage1").unwrap();
        assert_eq!(info.ref_count, 2);
        
        // 对于不存在的哈希，增加引用应该创建新的映射
        dedup.add_hash_reference("hash2", "storage2");
        assert_eq!(dedup.hash_to_storage.get("hash2"), Some(&"storage2".to_string()));
    }
}
