//! 本地共享缓存（默认 `cache.json`，与 `config.toml` 同目录）。
//!
//! 文件里有两个互不影响的区段：
//!
//! ```json
//! {
//!   "voices": {
//!     "qwen-audio-3.1-tts-flash": { "<音频sha256>": { "voice_id": "...", ... } }
//!   },
//!   "asr_hotwords": {
//!     "qwen-audio-3.1-asr-flash-streaming": {
//!       "vocabulary_id": "vocab-bivox-...",
//!       "hotwords": ["原神"],
//!       "weight": 50,
//!       "updated_at": 1730000000
//!     }
//!   }
//! }
//! ```
//!
//! - `voices`：声音复刻缓存（D11）。同一段样例音频在不同 TTS 模型下分别缓存，
//!   同一模型下同一音频只上传/复刻一次；
//! - `asr_hotwords`：ASR 预编译热词（D31）。按 ASR 模型保存百炼返回的
//!   `vocabulary_id`、当时上传的热词快照与最后更新时间，供下次会话复用/判断是否需要更新。
//!
//! 该文件是本机状态；如果格式变化导致旧文件解析失败，会被备份并自动重建，不做旧格式兼容。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 缓存文件内容。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheFile {
    /// TTS 模型 → 样例音频 SHA256（小写十六进制）→ 复刻音色记录。
    #[serde(default)]
    pub voices: BTreeMap<String, BTreeMap<String, VoiceCacheEntry>>,

    /// ASR 模型 → 预编译热词列表。
    #[serde(default)]
    pub asr_hotwords: BTreeMap<String, HotwordCacheEntry>,
}

/// 一条「样例音频 → 百炼音色」缓存记录（模型由外层键标识）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceCacheEntry {
    /// 百炼返回的复刻音色 ID，合成时作为 `voice` 参数。
    pub voice_id: String,

    /// 该声线样本的语言（声音复刻语言代码，仅用于展示/管理）。
    #[serde(default)]
    pub language: String,

    /// 配置文件里的自定义音色名 / 显示名，仅用于展示。
    #[serde(default)]
    pub name: String,

    /// 样例音频路径（展示与排查用，不参与匹配）。
    #[serde(default)]
    pub audio_path: String,

    /// 创建时间（Unix 秒）。
    #[serde(default)]
    pub created_at: u64,
}

/// 一条「ASR 模型 → 预编译热词列表」缓存记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HotwordCacheEntry {
    /// 百炼返回的预编译热词列表 ID，识别时作为 `vocabulary_id` 参数。
    pub vocabulary_id: String,

    /// 该列表当前包含的热词快照（归一化后的顺序）；与配置不一致时需要更新。
    #[serde(default)]
    pub hotwords: Vec<String>,

    /// 该列表创建/更新时使用的热词权重（本项目固定 50）。
    ///
    /// 记录它可以让「代码里的默认权重变化」也能触发一次列表更新；
    /// 旧缓存没有该字段时按 0 读取，与当前权重不一致 → 下次会更新。
    #[serde(default)]
    pub weight: u32,

    /// 最后一次创建/更新的时间（Unix 秒）。
    ///
    /// 百炼建议两次热词更新至少间隔 5 分钟，本字段用于客户端限速。
    #[serde(default)]
    pub updated_at: u64,
}

/// 缓存文件的读写封装。先在内存里改，完成后再 [`AppCache::save`] 落盘。
#[derive(Debug)]
pub struct AppCache {
    path: PathBuf,
    file: CacheFile,
}

impl AppCache {
    /// 读取缓存文件；文件不存在时返回空缓存（不会立即创建文件）。
    ///
    /// 文件损坏或格式已变化（解析失败）时先备份为 `.bak` 再以空缓存继续，
    /// 之后写入时自动按当前格式重建。
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let file = if path.exists() {
            match read_cache_file(&path) {
                Ok(file) => file,
                Err(e) => {
                    let backup = path.with_extension("json.bak");
                    let backup_note = match std::fs::copy(&path, &backup) {
                        Ok(_) => format!("已备份为 {}", backup.display()),
                        Err(copy_err) => format!("备份失败: {copy_err}"),
                    };
                    log::warn!(
                        "⚠️ 本地缓存文件 {} 无法读取（{e:#}），{backup_note}；将重新建立缓存",
                        path.display()
                    );
                    CacheFile::default()
                }
            }
        } else {
            CacheFile::default()
        };
        Ok(Self { path, file })
    }

    /// 音色缓存记录数量（所有模型合计）。
    pub fn len(&self) -> usize {
        self.file.voices.values().map(BTreeMap::len).sum()
    }

    /// 是否没有音色缓存记录（热词缓存不参与该判断）。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 按「模型 + 音频内容 SHA256」查询音色缓存。
    pub fn get(&self, target_model: &str, audio_hash: &str) -> Option<&VoiceCacheEntry> {
        self.file.voices.get(target_model)?.get(audio_hash)
    }

    /// 按缓存记录里的样例音频路径查找，同时返回音频哈希（用于缓存恢复）。
    pub fn find_with_hash_by_audio_path(
        &self,
        target_model: &str,
        audio_path: &str,
    ) -> Option<(&str, &VoiceCacheEntry)> {
        self.file
            .voices
            .get(target_model)?
            .iter()
            .filter(|(_, entry)| !entry.voice_id.is_empty() && entry.audio_path == audio_path)
            .max_by_key(|(_, entry)| entry.created_at)
            .map(|(hash, entry)| (hash.as_str(), entry))
    }

    /// 写入/覆盖一条音色缓存（只在内存中，需调用 [`AppCache::save`] 落盘）。
    pub fn insert(
        &mut self,
        target_model: &str,
        audio_hash: impl Into<String>,
        entry: VoiceCacheEntry,
    ) {
        self.file
            .voices
            .entry(target_model.to_string())
            .or_default()
            .insert(audio_hash.into(), entry);
    }

    /// 按 ASR 模型查询预编译热词缓存。
    pub fn hotword_entry(&self, target_model: &str) -> Option<&HotwordCacheEntry> {
        self.file.asr_hotwords.get(target_model)
    }

    /// 写入/覆盖一条热词缓存（只在内存中，需调用 [`AppCache::save`] 落盘）。
    pub fn set_hotword_entry(&mut self, target_model: &str, entry: HotwordCacheEntry) {
        self.file
            .asr_hotwords
            .insert(target_model.to_string(), entry);
    }

    /// 把内存中的缓存原子写入磁盘（临时文件 + rename），并自动创建父目录。
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建缓存目录 {} 失败", parent.display()))?;
        }

        let json = serde_json::to_string_pretty(&self.file).context("序列化本地缓存失败")?;
        let tmp_path = self.path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json)
            .with_context(|| format!("写入缓存临时文件 {} 失败", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &self.path)
            .with_context(|| format!("保存本地缓存 {} 失败", self.path.display()))?;
        Ok(())
    }
}

fn read_cache_file(path: &Path) -> Result<CacheFile> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取缓存文件 {} 失败", path.display()))?;
    let file: CacheFile = serde_json::from_str(&text)
        .with_context(|| format!("解析缓存 JSON {} 失败", path.display()))?;
    Ok(file)
}

/// 计算音频内容的 SHA256（小写十六进制）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

/// 当前 Unix 时间戳（秒）。
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache_path(tag: &str) -> PathBuf {
        let unique = format!(
            "bivox-cache-{tag}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique)
    }

    fn entry(voice_id: &str) -> VoiceCacheEntry {
        VoiceCacheEntry {
            voice_id: voice_id.to_string(),
            language: "zh".to_string(),
            name: "Kaoet".to_string(),
            audio_path: "samples/kaoet.wav".to_string(),
            created_at: unix_now(),
        }
    }

    fn hotword_entry(vocabulary_id: &str, hotwords: &[&str]) -> HotwordCacheEntry {
        HotwordCacheEntry {
            vocabulary_id: vocabulary_id.to_string(),
            hotwords: hotwords.iter().map(|word| (*word).to_string()).collect(),
            weight: 50,
            updated_at: unix_now(),
        }
    }

    const MODEL_A: &str = "qwen-audio-3.1-tts-flash";
    const MODEL_B: &str = "cosyvoice-v3.5-flash";
    const ASR_MODEL: &str = "qwen-audio-3.1-asr-flash-streaming";

    #[test]
    fn missing_cache_file_starts_empty_without_creating_file() {
        let path = temp_cache_path("missing");
        let cache = AppCache::load(path.clone()).unwrap();
        assert!(cache.is_empty());
        assert!(cache.hotword_entry(ASR_MODEL).is_none());
        assert!(!path.exists(), "读取空缓存不应创建文件");
    }

    #[test]
    fn cache_round_trips_through_disk() {
        let path = temp_cache_path("roundtrip");
        let hash = "a".repeat(64);

        let mut cache = AppCache::load(path.clone()).unwrap();
        cache.insert(MODEL_A, hash.clone(), entry("voice-abc"));
        cache.save().unwrap();

        let reloaded = AppCache::load(path.clone()).unwrap();
        let hit = reloaded.get(MODEL_A, &hash).expect("缓存应能命中");
        assert_eq!(hit.voice_id, "voice-abc");
        assert_eq!(hit.name, "Kaoet");
        assert_eq!(reloaded.len(), 1);
        assert!(
            reloaded.get(MODEL_B, &hash).is_none(),
            "不同模型之间不应串用缓存"
        );

        std::fs::remove_file(&path).ok();
    }

    /// 同一段音频在不同模型下各自缓存，互不覆盖（本次改动的核心目的）。
    #[test]
    fn same_audio_is_cached_separately_per_model() {
        let path = temp_cache_path("per-model");
        let hash = sha256_hex(b"same audio content");

        let mut cache = AppCache::load(path.clone()).unwrap();
        cache.insert(MODEL_A, hash.clone(), entry("voice-on-a"));
        cache.insert(MODEL_B, hash.clone(), entry("voice-on-b"));
        cache.save().unwrap();

        let reloaded = AppCache::load(path.clone()).unwrap();
        assert_eq!(reloaded.len(), 2, "两个模型各有一条记录");
        assert_eq!(reloaded.get(MODEL_A, &hash).unwrap().voice_id, "voice-on-a");
        assert_eq!(reloaded.get(MODEL_B, &hash).unwrap().voice_id, "voice-on-b");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn corrupt_cache_is_backed_up_and_reset() {
        let path = temp_cache_path("corrupt");
        std::fs::write(&path, b"{ not valid json").unwrap();

        let cache = AppCache::load(path.clone()).unwrap();
        assert!(cache.is_empty());
        assert!(path.with_extension("json.bak").exists(), "损坏文件应备份");

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(path.with_extension("json.bak")).ok();
    }

    #[test]
    fn hotword_entry_round_trips_through_disk() {
        let path = temp_cache_path("hotwords");
        let mut cache = AppCache::load(path.clone()).unwrap();
        cache.set_hotword_entry(ASR_MODEL, hotword_entry("vocab-1", &["原神", "Kaoet"]));
        cache.save().unwrap();

        let reloaded = AppCache::load(path.clone()).unwrap();
        let entry = reloaded.hotword_entry(ASR_MODEL).expect("应命中热词缓存");
        assert_eq!(entry.vocabulary_id, "vocab-1");
        assert_eq!(
            entry.hotwords,
            vec!["原神".to_string(), "Kaoet".to_string()]
        );
        assert_eq!(entry.weight, 50);
        assert!(reloaded.is_empty(), "只有热词缓存时音色缓存仍为空");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn old_hotword_entry_without_weight_reads_as_zero() {
        // 兼容旧缓存格式（没有 weight 字段）：按 0 读取，运行期会据此触发一次更新。
        let path = temp_cache_path("hotwords-old");
        std::fs::write(
            &path,
            r#"{"asr_hotwords":{"qwen-audio-3.1-asr-flash-streaming":{"vocabulary_id":"v1","hotwords":["原神"],"updated_at":1}}}"#,
        )
        .unwrap();

        let cache = AppCache::load(path.clone()).unwrap();
        let entry = cache.hotword_entry(ASR_MODEL).unwrap();
        assert_eq!(entry.weight, 0, "旧缓存缺少 weight 时按 0 读取");
        assert_eq!(entry.vocabulary_id, "v1");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn hotword_entry_can_be_replaced() {
        let mut cache = AppCache::load(temp_cache_path("hotwords-update")).unwrap();
        cache.set_hotword_entry(ASR_MODEL, hotword_entry("vocab-1", &["旧词"]));
        cache.set_hotword_entry(ASR_MODEL, hotword_entry("vocab-2", &["新词"]));
        assert_eq!(
            cache.hotword_entry(ASR_MODEL).unwrap().vocabulary_id,
            "vocab-2"
        );
    }

    #[test]
    fn find_with_hash_by_audio_path_returns_latest_matching_entry() {
        let mut cache = AppCache::load(temp_cache_path("by-path")).unwrap();
        let mut older = entry("voice-old");
        older.audio_path = "samples/kaoet.wav".to_string();
        older.created_at = 100;
        let mut newer = entry("voice-new");
        newer.audio_path = "samples/kaoet.wav".to_string();
        newer.created_at = 200;
        let mut other_model = entry("voice-other-model");
        other_model.audio_path = "samples/kaoet.wav".to_string();
        other_model.created_at = 300;

        cache.insert(MODEL_A, "hash-old", older);
        cache.insert(MODEL_A, "hash-new", newer);
        cache.insert(MODEL_B, "hash-other-model", other_model);

        let (hash, found) = cache
            .find_with_hash_by_audio_path(MODEL_A, "samples/kaoet.wav")
            .unwrap();
        assert_eq!(hash, "hash-new", "应返回对应的音频哈希");
        assert_eq!(found.voice_id, "voice-new", "应取同模型下最新的一条");
        assert!(
            cache
                .find_with_hash_by_audio_path("model-z", "samples/kaoet.wav")
                .is_none()
        );
        assert!(
            cache
                .find_with_hash_by_audio_path(MODEL_A, "samples/other.wav")
                .is_none()
        );
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_hex(b"").len(), 64);
    }
}
