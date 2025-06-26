use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use anyhow::{Result, anyhow};
use crate::config::DeltaAlgorithm;

/// 差分存储管理器
/// 
/// 通过检测文件间的相似性，对相似文件使用差分存储技术，
/// 只存储差异部分，大幅减少存储空间。
#[derive(Debug)]
pub struct DeltaStorage {
    /// 基础文件存储 (storage_id -> 文件数据)
    base_files: HashMap<String, Vec<u8>>,
    /// 相似度阈值（0.0-1.0）
    similarity_threshold: f32,
    /// 差分算法
    delta_algorithm: DeltaAlgorithm,
    /// 基础文件的元信息
    base_file_info: HashMap<String, BaseFileInfo>,
}

/// 基础文件信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseFileInfo {
    /// 文件大小
    pub size: u64,
    /// 文件类型（通过扩展名推断）
    pub file_type: String,
    /// 创建时间
    pub created_at: u64,
    /// 被引用次数
    pub reference_count: u32,
}

/// 差分信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaInfo {
    /// 是否为差分文件
    pub is_delta: bool,
    /// 基础文件的存储ID
    pub base_storage_id: Option<String>,
    /// 与基础文件的相似度
    pub similarity_score: Option<f32>,
    /// 差分算法
    pub delta_algorithm: DeltaAlgorithm,
    /// 原始文件大小
    pub original_size: u64,
    /// 差分数据大小
    pub delta_size: u64,
}

/// 相似文件匹配结果
#[derive(Debug, Clone)]
pub struct SimilarityMatch {
    /// 基础文件的存储ID
    pub base_storage_id: String,
    /// 相似度分数（0.0-1.0）
    pub similarity_score: f32,
    /// 估计的压缩率
    pub estimated_compression: f32,
}

impl DeltaStorage {
    /// 创建新的差分存储管理器
    pub fn new(similarity_threshold: f32, delta_algorithm: DeltaAlgorithm) -> Self {
        Self {
            base_files: HashMap::new(),
            similarity_threshold,
            delta_algorithm,
            base_file_info: HashMap::new(),
        }
    }

    /// 计算两个文件的相似度
    /// 
    /// 使用滑动窗口算法计算相似度，返回0.0-1.0的分数
    pub fn calculate_similarity(&self, data1: &[u8], data2: &[u8]) -> f32 {
        if data1.is_empty() && data2.is_empty() {
            return 1.0;
        }
        if data1.is_empty() || data2.is_empty() {
            return 0.0;
        }

        // 对于短数据使用字节级比较，长数据使用窗口比较
        if data1.len() <= 16 || data2.len() <= 16 {
            return self.calculate_byte_similarity(data1, data2);
        }

        // 使用滑动窗口比较
        let window_size = std::cmp::min(8, std::cmp::min(data1.len(), data2.len()) / 4);
        if window_size == 0 {
            return self.calculate_byte_similarity(data1, data2);
        }

        let mut matches = 0;
        let mut total_windows = 0;

        // 在data1中滑动窗口
        for i in 0..=data1.len().saturating_sub(window_size) {
            total_windows += 1;
            let window1 = &data1[i..i + window_size];

            // 在data2中寻找匹配的窗口
            let mut found_match = false;
            for j in 0..=data2.len().saturating_sub(window_size) {
                let window2 = &data2[j..j + window_size];
                if window1 == window2 {
                    matches += 1;
                    found_match = true;
                    break;
                }
            }

            // 如果没有找到完全匹配，检查部分匹配
            if !found_match {
                let mut best_partial_match = 0;
                for j in 0..=data2.len().saturating_sub(window_size) {
                    let window2 = &data2[j..j + window_size];
                    let partial_matches = window1.iter()
                        .zip(window2.iter())
                        .filter(|(a, b)| a == b)
                        .count();
                    best_partial_match = best_partial_match.max(partial_matches);
                }
                
                // 部分匹配按比例计算
                if best_partial_match > window_size / 2 {
                    matches += best_partial_match / window_size;
                }
            }
        }

        if total_windows == 0 {
            0.0
        } else {
            matches as f32 / total_windows as f32
        }
    }

    /// 计算字节级相似度（用于短数据）
    fn calculate_byte_similarity(&self, data1: &[u8], data2: &[u8]) -> f32 {
        let max_len = std::cmp::max(data1.len(), data2.len());
        if max_len == 0 {
            return 1.0;
        }

        let min_len = std::cmp::min(data1.len(), data2.len());
        let matches = data1.iter()
            .take(min_len)
            .zip(data2.iter().take(min_len))
            .filter(|(a, b)| a == b)
            .count();

        matches as f32 / max_len as f32
    }

    /// 寻找最相似的基础文件
    pub fn find_best_base(&self, data: &[u8], file_type: &str) -> Option<SimilarityMatch> {
        let mut best_match = None;
        let mut best_similarity = 0.0;

        for (base_id, base_data) in &self.base_files {
            // 优先匹配相同文件类型
            if let Some(base_info) = self.base_file_info.get(base_id) {
                let type_bonus = if base_info.file_type == file_type { 0.1 } else { 0.0 };
                
                let similarity = self.calculate_similarity(data, base_data) + type_bonus;
                
                if similarity > best_similarity && similarity >= self.similarity_threshold {
                    best_similarity = similarity;
                    
                    // 估计压缩率（基于相似度）
                    let estimated_compression = 1.0 - (1.0 - similarity) * 0.8;
                    
                    best_match = Some(SimilarityMatch {
                        base_storage_id: base_id.clone(),
                        similarity_score: similarity,
                        estimated_compression,
                    });
                }
            }
        }

        best_match
    }

    /// 创建差分数据
    pub fn create_delta(&self, base_data: &[u8], target_data: &[u8]) -> Result<Vec<u8>> {
        match self.delta_algorithm {
            DeltaAlgorithm::Simple => self.create_simple_delta(base_data, target_data),
            DeltaAlgorithm::XDelta => {
                // TODO: 实现xdelta3算法
                Err(anyhow!("XDelta algorithm not implemented yet"))
            }
            DeltaAlgorithm::BsDiff => {
                // TODO: 实现bsdiff算法
                Err(anyhow!("BsDiff algorithm not implemented yet"))
            }
        }
    }

    /// 简单差分算法实现
    fn create_simple_delta(&self, base_data: &[u8], target_data: &[u8]) -> Result<Vec<u8>> {
        let mut delta = Vec::new();
        
        // 写入头部信息
        delta.extend_from_slice(b"STOWR_DELTA_V1");
        delta.extend_from_slice(&(base_data.len() as u64).to_le_bytes());
        delta.extend_from_slice(&(target_data.len() as u64).to_le_bytes());
        
        // 简单的逐字节差分
        let mut i = 0;
        while i < target_data.len() {
            if i < base_data.len() && target_data[i] == base_data[i] {
                // 相同字节，记录连续相同的长度
                let mut same_count = 0;
                while i + same_count < target_data.len() 
                    && i + same_count < base_data.len() 
                    && target_data[i + same_count] == base_data[i + same_count] {
                    same_count += 1;
                }
                
                // 写入COPY指令
                delta.push(0x01); // COPY command
                delta.extend_from_slice(&(same_count as u32).to_le_bytes());
                i += same_count;
            } else {
                // 不同字节，记录需要插入的数据
                let diff_start = i;
                while i < target_data.len() 
                    && (i >= base_data.len() || target_data[i] != base_data[i]) {
                    i += 1;
                }
                
                let diff_len = i - diff_start;
                // 写入INSERT指令
                delta.push(0x02); // INSERT command
                delta.extend_from_slice(&(diff_len as u32).to_le_bytes());
                delta.extend_from_slice(&target_data[diff_start..i]);
            }
        }

        Ok(delta)
    }

    /// 应用差分数据重建原文件
    pub fn apply_delta(&self, base_data: &[u8], delta_data: &[u8]) -> Result<Vec<u8>> {
        if delta_data.len() < 22 { // 最小头部大小
            return Err(anyhow!("Invalid delta data: too short"));
        }

        // 检查头部
        if &delta_data[0..14] != b"STOWR_DELTA_V1" {
            return Err(anyhow!("Invalid delta data: wrong header"));
        }

        let base_len = u64::from_le_bytes(
            delta_data[14..22].try_into().map_err(|_| anyhow!("Invalid base length"))?
        ) as usize;
        let target_len = u64::from_le_bytes(
            delta_data[22..30].try_into().map_err(|_| anyhow!("Invalid target length"))?
        ) as usize;

        if base_data.len() != base_len {
            return Err(anyhow!("Base data length mismatch"));
        }

        let mut result = Vec::with_capacity(target_len);
        let mut delta_pos = 30;
        let mut base_pos = 0;

        while delta_pos < delta_data.len() {
            let command = delta_data[delta_pos];
            delta_pos += 1;

            match command {
                0x01 => { // COPY
                    if delta_pos + 4 > delta_data.len() {
                        return Err(anyhow!("Invalid COPY command"));
                    }
                    let copy_len = u32::from_le_bytes(
                        delta_data[delta_pos..delta_pos + 4].try_into().unwrap()
                    ) as usize;
                    delta_pos += 4;

                    if base_pos + copy_len > base_data.len() {
                        return Err(anyhow!("COPY command out of bounds"));
                    }

                    result.extend_from_slice(&base_data[base_pos..base_pos + copy_len]);
                    base_pos += copy_len;
                }
                0x02 => { // INSERT
                    if delta_pos + 4 > delta_data.len() {
                        return Err(anyhow!("Invalid INSERT command"));
                    }
                    let insert_len = u32::from_le_bytes(
                        delta_data[delta_pos..delta_pos + 4].try_into().unwrap()
                    ) as usize;
                    delta_pos += 4;

                    if delta_pos + insert_len > delta_data.len() {
                        return Err(anyhow!("INSERT command out of bounds"));
                    }

                    result.extend_from_slice(&delta_data[delta_pos..delta_pos + insert_len]);
                    delta_pos += insert_len;
                }
                _ => return Err(anyhow!("Unknown delta command: {}", command)),
            }
        }

        if result.len() != target_len {
            return Err(anyhow!("Reconstructed file size mismatch"));
        }

        Ok(result)
    }

    /// 添加基础文件
    pub fn add_base_file(&mut self, storage_id: String, data: Vec<u8>, file_type: String) {
        let info = BaseFileInfo {
            size: data.len() as u64,
            file_type,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            reference_count: 0,
        };

        self.base_files.insert(storage_id.clone(), data);
        self.base_file_info.insert(storage_id, info);
    }

    /// 移除基础文件
    pub fn remove_base_file(&mut self, storage_id: &str) -> bool {
        if let Some(info) = self.base_file_info.get(storage_id) {
            if info.reference_count == 0 {
                self.base_files.remove(storage_id);
                self.base_file_info.remove(storage_id);
                true
            } else {
                false // 还有引用，不能删除
            }
        } else {
            // 没有找到，可能已经被删除
            self.base_files.remove(storage_id);
            true
        }
    }

    /// 增加基础文件的引用计数
    pub fn increment_reference(&mut self, storage_id: &str) {
        if let Some(info) = self.base_file_info.get_mut(storage_id) {
            info.reference_count += 1;
        }
    }

    /// 减少基础文件的引用计数
    pub fn decrement_reference(&mut self, storage_id: &str) -> bool {
        if let Some(info) = self.base_file_info.get_mut(storage_id) {
            if info.reference_count > 0 {
                info.reference_count -= 1;
            }
            info.reference_count == 0
        } else {
            true // 如果找不到信息，默认可以删除
        }
    }

    /// 获取基础文件数据
    pub fn get_base_file_data(&self, storage_id: &str) -> Option<&[u8]> {
        self.base_files.get(storage_id).map(|v| v.as_slice())
    }

    /// 获取差分存储统计信息
    pub fn get_stats(&self) -> DeltaStats {
        let total_base_files = self.base_files.len() as u32;
        let total_references = self.base_file_info.values()
            .map(|info| info.reference_count)
            .sum::<u32>();

        DeltaStats {
            total_base_files,
            total_delta_files: total_references,
            average_similarity: 0.0, // TODO: 计算平均相似度
            storage_savings: 0.0,    // TODO: 计算存储节省
        }
    }

    /// 推断文件类型
    pub fn infer_file_type(file_path: &std::path::Path) -> String {
        file_path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("unknown")
            .to_lowercase()
    }
}

/// 差分存储统计信息
#[derive(Debug, Clone)]
pub struct DeltaStats {
    /// 基础文件数量
    pub total_base_files: u32,
    /// 差分文件数量
    pub total_delta_files: u32,
    /// 平均相似度
    pub average_similarity: f32,
    /// 存储空间节省率
    pub storage_savings: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_similarity_calculation() {
        let delta_storage = DeltaStorage::new(0.7, DeltaAlgorithm::Simple);
        
        let data1 = b"Hello World";
        let data2 = b"Hello World";
        let data3 = b"Hello Rust";
        
        // 完全相同
        let identical_similarity = delta_storage.calculate_similarity(data1, data2);
        assert!((identical_similarity - 1.0).abs() < 0.1, "Identical files should have similarity close to 1.0, got: {}", identical_similarity);
        
        // 部分相似
        let partial_similarity = delta_storage.calculate_similarity(data1, data3);
        assert!(partial_similarity >= 0.0 && partial_similarity <= 1.0, "Similarity should be between 0.0 and 1.0, got: {}", partial_similarity);
        
        // 测试更相似的字符串
        let similar_data1 = b"Hello World Test";
        let similar_data2 = b"Hello World Best";
        let similar_similarity = delta_storage.calculate_similarity(similar_data1, similar_data2);
        assert!(similar_similarity > 0.0, "Similar texts should have similarity > 0.0, got: {}", similar_similarity);
        
        // 测试完全不同的字符串
        let diff_data1 = b"AAAAAAAAAA";
        let diff_data2 = b"BBBBBBBBBB";
        let diff_similarity = delta_storage.calculate_similarity(diff_data1, diff_data2);
        assert!(diff_similarity == 0.0, "Completely different data should have similarity 0.0, got: {}", diff_similarity);
    }

    #[test]
    fn test_simple_delta() {
        let delta_storage = DeltaStorage::new(0.7, DeltaAlgorithm::Simple);
        
        let base_data = b"Hello World";
        let target_data = b"Hello Rust World";
        
        let delta = delta_storage.create_delta(base_data, target_data).unwrap();
        let reconstructed = delta_storage.apply_delta(base_data, &delta).unwrap();
        
        assert_eq!(reconstructed, target_data);
    }

    #[test]
    fn test_file_type_inference() {
        use std::path::Path;
        
        assert_eq!(DeltaStorage::infer_file_type(Path::new("test.txt")), "txt");
        assert_eq!(DeltaStorage::infer_file_type(Path::new("image.png")), "png");
        assert_eq!(DeltaStorage::infer_file_type(Path::new("noext")), "unknown");
    }
}
