//! 自定义（复刻）音色。
//!
//! 配置里通过 `[tts] voice = "custom:<名字>"` 引用 `[tts.custom_voices.<名字>]`。
//! 一个音色可以配置多种语言的声线样本（`[[...samples]]` 有序数组）：
//! 每个样本都会按 (`[tts] model`, 音频内容哈希) 单独复刻并缓存一个 voice_id；
//! 合成时由 pipeline 判定本句语言，选择对应语言的声线（连同该样本配置的语速），
//! 缺语言时回退到第一个样本（D11/D23）。

pub mod admin;
pub mod api;
pub mod editor;

#[cfg(test)]
pub(crate) mod mock_http;

use anyhow::{Context, Result, bail};
use colored::Colorize;
use futures_util::future::join_all;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use crate::cache::{self, AppCache, VoiceCacheEntry};
use crate::config::{
    AppConfig, CustomVoiceConfig, CustomVoiceSample, DEFAULT_SAMPLE_RATE, MAX_VOICE_PREFIX_LEN,
};
use crate::language;
use api::BailianVoiceApi;

/// 创建音色后等待百炼部署就绪的最长时间。
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// 百炼样例音频大小上限（10 MB）。
const MAX_AUDIO_BYTES: usize = 10 * 1024 * 1024;

/// 复刻时使用的参考音频最大时长（秒），官方 `max_prompt_audio_length`（D30）。
///
/// 官方范围 [3.0, 30.0]、默认 **10.0**；默认值会把「推荐 10~20 秒」样本的后半截静默丢掉，
/// 因此显式取 20 秒，与 README/配置模板的推荐区间一致。样例音频超过该时长时，
/// 只有前 20 秒（预处理后）参与复刻。
const MAX_PROMPT_AUDIO_LENGTH: f32 = 20.0;

/// 当前生效的音色：系统音色，或带多语言声线样本的自定义音色。
#[derive(Debug, Clone)]
pub enum ActiveVoice {
    /// 系统音色 / 直接指定的 voice_id：所有语言共用同一个 voice。
    Builtin { name: String },
    /// 自定义音色：按语言选择声线样本。
    Custom(ResolvedCustomVoice),
}

/// 已解析完成的单个语言声线：百炼 voice_id + 该样本配置的语速倍率。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSample {
    pub voice_id: String,
    /// 该样本的语速倍率（百炼 TTS `rate`，0.5–2.0，默认 1.0）。
    pub rate: f32,
}

/// 已解析完成的自定义音色（所有样本的 voice_id 均已就绪）。
#[derive(Debug, Clone)]
pub struct ResolvedCustomVoice {
    pub display_name: String,
    /// 默认声线样本的语言（`samples` 第一个，声音复刻语言代码）。
    pub default_language: String,
    /// 语言（canonical 复刻代码）→ 声线（voice_id + 语速）。
    pub samples: HashMap<String, ResolvedSample>,
}

impl ActiveVoice {
    /// 按本句语言返回应使用的百炼 voice 与语速倍率。
    pub fn selection_for(&self, asr_language: &str) -> (&str, f32) {
        match self {
            Self::Builtin { name } => (name.as_str(), DEFAULT_SAMPLE_RATE),
            Self::Custom(custom) => custom.selection_for(asr_language),
        }
    }

    /// 按本句语言返回应使用的百炼 voice（语速见 [`Self::selection_for`]）。
    pub fn voice_for(&self, asr_language: &str) -> String {
        self.selection_for(asr_language).0.to_string()
    }
}

impl ResolvedCustomVoice {
    /// 按本句语言选择声线：无该语言样本时回退到 `samples[0]`（默认声线）。
    pub fn sample_for(&self, asr_language: &str) -> Option<(&str, &ResolvedSample)> {
        let canonical = language::canonical_language(asr_language);
        self.samples
            .get_key_value(canonical)
            .or_else(|| self.samples.get_key_value(&self.default_language))
            .or_else(|| self.samples.iter().next())
            .map(|(language, sample)| (language.as_str(), sample))
    }

    /// 按本句语言返回（voice_id, rate）：无样本时返回空 voice + 默认语速。
    pub fn selection_for(&self, asr_language: &str) -> (&str, f32) {
        self.sample_for(asr_language)
            .map(|(_, sample)| (sample.voice_id.as_str(), sample.rate))
            .unwrap_or(("", DEFAULT_SAMPLE_RATE))
    }

    /// 按本句语言选择 voice_id：无该语言样本时回退到 `samples[0]`（默认声线）。
    pub fn voice_for(&self, asr_language: &str) -> &str {
        self.sample_for(asr_language)
            .map(|(_, sample)| sample.voice_id.as_str())
            .unwrap_or_default()
    }

    /// 已配置的语言（按字典序，便于展示）。
    pub fn languages(&self) -> Vec<&str> {
        let mut languages: Vec<&str> = self.samples.keys().map(String::as_str).collect();
        languages.sort_unstable();
        languages
    }
}

/// 根据配置构造百炼音色 API 客户端。
pub fn api_from_config(config: &AppConfig) -> Result<BailianVoiceApi> {
    let api_key = config
        .api
        .key
        .clone()
        .context("未配置 API Key，请在配置文件的 [api] key 中设置")?;
    BailianVoiceApi::new(api_key, config.http_api_base())
}

/// 读取配置对应的本地缓存。
pub fn cache_from_config(config: &AppConfig) -> Result<AppCache> {
    AppCache::load(config.cache_path())
}

/// 解析当前生效的音色。
///
/// - 普通系统音色 / 音色 ID：原样返回（不访问网络）。
/// - `custom:<名字>`：确保该音色全部语言样本都已注册，返回可选声线映射。
pub async fn resolve_active_voice(config: &AppConfig) -> Result<ActiveVoice> {
    let Some((name, custom)) = config.active_custom_voice() else {
        return Ok(ActiveVoice::Builtin {
            name: config.tts.voice.clone(),
        });
    };

    let api = api_from_config(config)?;
    let mut cache = cache_from_config(config)?;
    let resolved = resolve_custom_voice(config, &api, &mut cache, name, custom, None).await?;
    Ok(ActiveVoice::Custom(resolved))
}

/// 确保配置中的自定义音色样本全部可用（必要时并行上传复刻），返回语言→voice_id 映射。
///
/// `only_language`：只处理指定语言的样本（当前调用方传 `None`）。
pub async fn resolve_custom_voice(
    config: &AppConfig,
    api: &BailianVoiceApi,
    cache: &mut AppCache,
    name: &str,
    custom: &CustomVoiceConfig,
    only_language: Option<&str>,
) -> Result<ResolvedCustomVoice> {
    let display_name = custom.display_name(name);
    let samples: Vec<&CustomVoiceSample> = match only_language {
        Some(language_code) => {
            let sample = custom.sample_for_language(language_code).with_context(|| {
                format!("自定义音色 [{name}] 没有语言 {language_code:?} 的声线样本")
            })?;
            vec![sample]
        }
        None => custom.samples.iter().collect(),
    };
    if samples.is_empty() {
        bail!("自定义音色 [{name}] 没有可用的声线样本");
    }

    // 1. 读取/校验所有样本音频（读不到时尝试用缓存里同路径的记录兜底）
    let mut prepared: Vec<PreparedSample> = Vec::with_capacity(samples.len());
    for sample in &samples {
        let audio_path = config.resolve_path(&sample.audio);
        prepared
            .push(prepare_sample(config, cache, name, &display_name, sample, &audio_path).await?);
    }

    // 2. 按音频内容哈希分组（多个语言共用同一段音频时只复刻一次）
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut pre_resolved: HashMap<usize, (String, String)> = HashMap::new(); // index -> (hash, voice_id)
    for (index, sample) in prepared.iter().enumerate() {
        match &sample.source {
            PreparedSource::Bytes { hash, .. } => {
                groups.entry(hash.clone()).or_default().push(index);
            }
            PreparedSource::Cached { hash, entry } => {
                log::info!(
                    "🎙️ 自定义音色「{}」语言 {} 命中缓存（样例音频不可读，复用原路径记录）: {}",
                    display_name,
                    sample.canonical_language,
                    entry.voice_id.cyan()
                );
                pre_resolved.insert(index, (hash.clone(), entry.voice_id.clone()));
            }
        }
    }

    // 3. 命中缓存的先校验/复用，未命中的并行注册
    let multi_language = custom.samples.len() > 1;
    let mut entries: HashMap<String, VoiceCacheEntry> = HashMap::new();
    let mut enroll_jobs = Vec::new();

    for (hash, indexes) in &groups {
        let first = &prepared[indexes[0]];
        let canonical_language = first.canonical_language.clone();
        let (bytes, file_name) = match &first.source {
            PreparedSource::Bytes {
                bytes, file_name, ..
            } => (bytes.clone(), file_name.clone()),
            PreparedSource::Cached { .. } => continue,
        };
        let prefix = sample_prefix(name, multi_language.then_some(canonical_language.as_str()));

        if let Some(entry) = cache.get(&config.tts.model, hash).cloned() {
            if !entry.voice_id.is_empty() {
                match verify_cached_voice(api, &entry.voice_id).await {
                    CachedVoiceState::Usable => {
                        log::info!(
                            "🎙️ 自定义音色「{}」语言 {} 命中缓存: {}",
                            display_name,
                            canonical_language,
                            entry.voice_id.cyan()
                        );
                        entries.insert(hash.clone(), entry);
                        continue;
                    }
                    CachedVoiceState::Missing => log::info!(
                        "♻️ 自定义音色「{}」语言 {} 的缓存 voice_id 已不存在，将重新复刻",
                        display_name,
                        canonical_language
                    ),
                    CachedVoiceState::Unverified(reason) => {
                        log::warn!(
                            "⚠️ 无法校验音色 {} 是否仍有效（{reason}），先继续使用缓存",
                            entry.voice_id
                        );
                        entries.insert(hash.clone(), entry);
                        continue;
                    }
                }
            }
        }

        let model = config.tts.model.clone();
        let display = display_name.clone();
        let audio_path_text = first.audio_path_text.clone();
        enroll_jobs.push(async move {
            let entry = enroll_one(
                api,
                &model,
                &display,
                &canonical_language,
                &prefix,
                &audio_path_text,
                &file_name,
                bytes,
            )
            .await?;
            Ok::<(String, VoiceCacheEntry), anyhow::Error>((hash.clone(), entry))
        });
    }

    // 并行注册（首次多语言会同时创建多个音色）
    if !enroll_jobs.is_empty() {
        log::info!(
            "⬆️ 共有 {} 个语言样本需要注册，正在并行处理...",
            enroll_jobs.len()
        );
        let results = join_all(enroll_jobs).await;
        let mut first_error: Option<anyhow::Error> = None;
        for result in results {
            match result {
                Ok((hash, entry)) => {
                    entries.insert(hash, entry);
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        // 已成功的也写回缓存，避免下次重复创建
        for (hash, entry) in &entries {
            cache.insert(&config.tts.model, hash.clone(), entry.clone());
        }
        if !entries.is_empty() {
            cache.save()?;
        }
        if let Some(error) = first_error {
            return Err(error).context(format!("自定义音色「{display_name}」部分语言注册失败"));
        }
    } else {
        let mut dirty = false;
        for (hash, entry) in &entries {
            if cache.get(&config.tts.model, hash).is_none() {
                cache.insert(&config.tts.model, hash.clone(), entry.clone());
                dirty = true;
            }
        }
        if dirty {
            cache.save()?;
        }
    }

    // 4. 汇总语言 → 声线（voice_id + 语速）
    let mut samples: HashMap<String, ResolvedSample> = HashMap::new();
    for (index, sample) in prepared.iter().enumerate() {
        let voice_id = match &sample.source {
            PreparedSource::Bytes { hash, .. } => entries
                .get(hash)
                .map(|entry| entry.voice_id.clone())
                .or_else(|| {
                    pre_resolved
                        .get(&index)
                        .map(|(_, voice_id)| voice_id.clone())
                })
                .with_context(|| {
                    format!(
                        "自定义音色「{display_name}」语言 {} 的 voice_id 缺失",
                        sample.canonical_language
                    )
                })?,
            PreparedSource::Cached { .. } => pre_resolved
                .get(&index)
                .map(|(_, voice_id)| voice_id.clone())
                .context("缓存兜底记录缺失")?,
        };
        samples.insert(
            sample.canonical_language.clone(),
            ResolvedSample {
                voice_id,
                rate: sample.rate,
            },
        );
    }

    let default_language =
        language::canonical_language(&prepared[0].canonical_language).to_string();
    let mut languages: Vec<String> = samples.keys().cloned().collect();
    languages.sort_unstable();
    log::info!(
        "🎙️ 自定义音色「{}」已就绪，语言声线: {}（默认 {}）",
        display_name,
        languages.join(", ").cyan(),
        default_language
    );

    Ok(ResolvedCustomVoice {
        display_name,
        default_language,
        samples,
    })
}

/// 一个已读取/已从缓存兜底的样本。
struct PreparedSample {
    canonical_language: String,
    audio_path_text: String,
    /// 该样本配置的语速倍率（与音频内容无关，只影响合成参数）。
    rate: f32,
    source: PreparedSource,
}

enum PreparedSource {
    Bytes {
        bytes: Vec<u8>,
        hash: String,
        file_name: String,
    },
    Cached {
        hash: String,
        entry: VoiceCacheEntry,
    },
}

async fn prepare_sample(
    config: &AppConfig,
    cache: &AppCache,
    config_name: &str,
    display_name: &str,
    sample: &CustomVoiceSample,
    audio_path: &Path,
) -> Result<PreparedSample> {
    let canonical_language = language::canonical_language(&sample.language).to_string();
    let audio_path_text = audio_path.display().to_string();

    // 读文件前先按元数据做大小预检：避免误指向超大文件时先读入内存（可能 OOM）。
    if let Ok(metadata) = std::fs::metadata(audio_path) {
        if metadata.len() > MAX_AUDIO_BYTES as u64 {
            bail!(
                "自定义音色「{display_name}」语言 {canonical_language} 的样例音频 {:.1} MB 超过百炼上限 10 MB，请先裁剪或压缩",
                metadata.len() as f64 / (1024.0 * 1024.0)
            );
        }
    }

    let bytes = match std::fs::read(audio_path) {
        Ok(bytes) => bytes,
        Err(read_err) => {
            // 音频文件被删除/移动时，只要缓存里有同一路径的已注册音色，仍可继续使用
            if let Some((hash, entry)) =
                cache.find_with_hash_by_audio_path(&config.tts.model, &audio_path_text)
            {
                log::warn!(
                    "⚠️ 读取样例音频 {} 失败（{read_err}），但缓存中有该路径对应的语言 {} 音色，\
                     继续复用 voice_id: {}（未校验云端状态；如需替换音频请恢复该文件）",
                    audio_path.display(),
                    canonical_language,
                    entry.voice_id.cyan()
                );
                return Ok(PreparedSample {
                    canonical_language,
                    audio_path_text,
                    rate: sample.rate,
                    source: PreparedSource::Cached {
                        hash: hash.to_string(),
                        entry: entry.clone(),
                    },
                });
            }
            return Err(read_err).with_context(|| {
                format!(
                    "读取自定义音色「{display_name}」语言 {canonical_language} 的样例音频 {} 失败；\
                     请检查 [tts.custom_voices.{config_name}.samples] 中的 audio 配置",
                    audio_path.display()
                )
            });
        }
    };
    if bytes.is_empty() {
        bail!(
            "自定义音色「{display_name}」语言 {canonical_language} 的样例音频 {} 是空文件",
            audio_path.display()
        );
    }
    if bytes.len() > MAX_AUDIO_BYTES {
        // 元数据预检之后文件又被替换的兜底
        bail!(
            "自定义音色「{display_name}」语言 {canonical_language} 的样例音频 {:.1} MB 超过百炼上限 10 MB，请先裁剪或压缩",
            bytes.len() as f64 / (1024.0 * 1024.0)
        );
    }
    check_audio_duration(&bytes, display_name, &canonical_language)?;

    let hash = cache::sha256_hex(&bytes);
    let file_name = audio_path
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .unwrap_or("sample")
        .to_string();
    Ok(PreparedSample {
        canonical_language,
        audio_path_text,
        rate: sample.rate,
        source: PreparedSource::Bytes {
            bytes,
            hash,
            file_name,
        },
    })
}

/// 上传一个样本并创建复刻音色，返回缓存记录。
#[allow(clippy::too_many_arguments)]
async fn enroll_one(
    api: &BailianVoiceApi,
    target_model: &str,
    display_name: &str,
    language_code: &str,
    prefix: &str,
    audio_path_text: &str,
    file_name: &str,
    bytes: Vec<u8>,
) -> Result<VoiceCacheEntry> {
    log::info!(
        "⬆️ 自定义音色「{}」语言 {}：上传样例音频...",
        display_name,
        language_code
    );
    let uploaded = api
        .upload_audio(file_name, bytes)
        .await
        .with_context(|| format!("上传「{display_name}」语言 {language_code} 的样例音频失败"))?;
    log::info!(
        "  ⬆️ 已上传样例音频: {}（file_id: {}）",
        uploaded.name,
        uploaded.file_id
    );

    let enroll_result = async {
        let audio_url = api
            .file_download_url(&uploaded.file_id)
            .await
            .with_context(|| format!("获取上传文件 {} 的下载地址失败", uploaded.file_id))?;
        log::info!(
            "🎨 自定义音色「{}」语言 {}：创建复刻音色（模型 {}，前缀 {}）...",
            display_name,
            language_code,
            target_model.cyan(),
            prefix.cyan()
        );
        let voice_id = api
            .create_voice(
                target_model,
                prefix,
                &audio_url,
                Some(language_code),
                MAX_PROMPT_AUDIO_LENGTH,
            )
            .await?;
        log::info!("⏳ 音色 {voice_id} 已创建，等待百炼完成复刻部署...");
        api.wait_until_ready(&voice_id, READY_TIMEOUT).await?;
        Ok::<String, anyhow::Error>(voice_id)
    }
    .await;

    if let Err(e) = api.delete_file(&uploaded.file_id).await {
        log::warn!(
            "⚠️ 清理百炼临时上传文件 {} 失败（不影响使用，可忽略）: {e:#}",
            uploaded.file_id
        );
    }
    let voice_id = enroll_result
        .with_context(|| format!("自定义音色「{display_name}」语言 {language_code} 注册失败"))?;
    log::info!(
        "{}",
        format!(
            "✅ 自定义音色「{}」语言 {} 已注册: {}",
            display_name, language_code, voice_id
        )
        .green()
    );

    Ok(VoiceCacheEntry {
        voice_id,
        language: language_code.to_string(),
        name: display_name.to_string(),
        audio_path: audio_path_text.to_string(),
        created_at: cache::unix_now(),
    })
}

/// 由配置键名与（多语言时的）语言代码派生百炼音色前缀。
///
/// 只保留 ASCII 小写字母/数字；超过 10 字符时优先保留语言后缀，先截短键名部分；
/// 净化后为空时回退 `voice`。
fn sample_prefix(config_name: &str, language_code: Option<&str>) -> String {
    let sanitize = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };

    let name = sanitize(config_name);
    let language = language_code.map(sanitize).unwrap_or_default();
    // 语言后缀优先保留：键名截到「10 - 语言代码长度」再拼语言代码
    let room = MAX_VOICE_PREFIX_LEN.saturating_sub(language.len());
    let mut prefix = format!(
        "{}{}",
        name.chars().take(room).collect::<String>(),
        language
    );
    if prefix.is_empty() {
        prefix = "voice".to_string();
    }
    prefix
}

/// WAV 样例音频的本地预检（官方音频要求 + 复刻时长上限，D30）。
fn check_audio_duration(bytes: &[u8], display_name: &str, language_code: &str) -> Result<()> {
    const MIN_SECONDS: f64 = 5.0;
    const RECOMMENDED_MAX_SECONDS: f64 = 60.0;

    let Ok(reader) = hound::WavReader::new(std::io::Cursor::new(bytes)) else {
        return Ok(());
    };
    let spec = reader.spec();
    if spec.sample_rate == 0 {
        return Ok(());
    }
    let seconds = f64::from(reader.duration()) / f64::from(spec.sample_rate);

    if seconds < MIN_SECONDS {
        bail!(
            "自定义音色「{display_name}」语言 {language_code} 的样例音频只有 {seconds:.1} 秒，太短了；\
             百炼要求有效音频不少于约 {MIN_SECONDS:.0} 秒（推荐 10~20 秒清晰人声，单条建议不超过 60 秒）"
        );
    }
    if seconds > RECOMMENDED_MAX_SECONDS {
        log::warn!(
            "⚠️ 自定义音色「{display_name}」语言 {language_code} 的样例音频较长（{seconds:.1} 秒），\
             建议裁剪到 10~20 秒（官方建议单条不超过 60 秒）以获得更好的复刻效果"
        );
    } else if seconds > f64::from(MAX_PROMPT_AUDIO_LENGTH) {
        log::info!(
            "ℹ️ 自定义音色「{display_name}」语言 {language_code} 的样例音频 {seconds:.1} 秒，\
             其中只有前 {MAX_PROMPT_AUDIO_LENGTH:.0} 秒会参与复刻"
        );
    }
    Ok(())
}

/// 缓存音色在百炼侧的有效性校验结果。
enum CachedVoiceState {
    Usable,
    Missing,
    Unverified(String),
}

/// 校验缓存 voice_id 是否仍存在于百炼账号。
async fn verify_cached_voice(api: &BailianVoiceApi, voice_id: &str) -> CachedVoiceState {
    match api.query_voice(voice_id).await {
        Ok(detail) if api::is_failed_status(&detail.status) => CachedVoiceState::Missing,
        Ok(_) => CachedVoiceState::Usable,
        Err(e) if api::is_resource_missing_error(&e) => CachedVoiceState::Missing,
        Err(e) => CachedVoiceState::Unverified(format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::mock_http::{MockHttpServer, MockResponse, RecordedRequest};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kaols-voice-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 生成 5 秒静音 WAV（用 marker 区分内容，得到不同哈希）。
    fn wav_bytes(marker: i16) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        {
            let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
            for index in 0..(5 * 16_000) {
                let sample = if index % 1600 == 0 { marker } else { 0 };
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        cursor.into_inner()
    }

    struct MockEnrollment {
        server: MockHttpServer,
        created: Arc<AtomicUsize>,
    }

    async fn mock_enrollment() -> MockEnrollment {
        let created = Arc::new(AtomicUsize::new(0));
        let created_server = created.clone();
        let server = MockHttpServer::start(move |request: &RecordedRequest| {
            match (request.method.as_str(), request.path.as_str()) {
                ("POST", "/api/v1/files") => MockResponse::json(
                    r#"{"data":{"uploaded_files":[{"name":"sample.wav","file_id":"file-1"}],"failed_uploads":[]}}"#,
                ),
                ("GET", "/api/v1/files/file-1") => {
                    MockResponse::json(r#"{"data":{"url":"https://oss/sample.wav"}}"#)
                }
                ("DELETE", "/api/v1/files/file-1") => {
                    MockResponse::json(r#"{"request_id":"ok"}"#)
                }
                _ => {
                    let body: serde_json::Value =
                        serde_json::from_str(&request.body_text()).unwrap_or_default();
                    match body["input"]["action"].as_str() {
                        Some("create_voice") => {
                            created_server.fetch_add(1, Ordering::SeqCst);
                            let prefix = body["input"]["prefix"].as_str().unwrap_or("p");
                            MockResponse::json(format!(
                                r#"{{"output":{{"voice_id":"voice-for-{prefix}"}}}}"#
                            ))
                        }
                        Some("query_voice") => {
                            MockResponse::json(r#"{"output":{"status":"OK"}}"#)
                        }
                        _ => MockResponse::json(r#"{"output":{}}"#),
                    }
                }
            }
        })
        .await;
        MockEnrollment { server, created }
    }

    fn config_with_two_languages(dir: &Path, cache_path: &Path) -> AppConfig {
        let zh = dir.join("edgar-zh.wav");
        let ja = dir.join("edgar-ja.wav");
        std::fs::write(&zh, wav_bytes(1)).unwrap();
        std::fs::write(&ja, wav_bytes(2)).unwrap();
        let text = format!(
            "[api]\nkey = \"sk-test\"\n[tts]\nvoice = \"custom:edgar\"\n[cache]\nfile = \"{}\"\n\
             [tts.custom_voices.edgar]\nname = \"埃德嘉\"\n\
             [[tts.custom_voices.edgar.samples]]\nlanguage = \"zh\"\naudio = \"{}\"\nrate = 1.25\n\
             [[tts.custom_voices.edgar.samples]]\nlanguage = \"ja\"\naudio = \"{}\"\n",
            cache_path.display().to_string().replace('\\', "\\\\"),
            zh.display().to_string().replace('\\', "\\\\"),
            ja.display().to_string().replace('\\', "\\\\"),
        );
        AppConfig::from_toml_str(&text).unwrap()
    }

    #[tokio::test]
    async fn resolves_multiple_language_samples_and_caches_them() {
        let dir = temp_dir("multi-lang");
        let cache_path = dir.join("cache.json");
        let config = config_with_two_languages(&dir, &cache_path);
        let mock = mock_enrollment().await;
        let api =
            BailianVoiceApi::new("sk-test", format!("{}/api/v1", mock.server.base_url())).unwrap();
        let mut cache = AppCache::load(&cache_path).unwrap();
        let (name, custom) = config.active_custom_voice().unwrap();

        let resolved = resolve_custom_voice(&config, &api, &mut cache, name, custom, None)
            .await
            .unwrap();
        assert_eq!(resolved.samples["zh"].voice_id, "voice-for-edgarzh");
        assert_eq!(resolved.samples["ja"].voice_id, "voice-for-edgarja");
        assert!((resolved.samples["zh"].rate - 1.25).abs() < 1e-6);
        assert_eq!(
            resolved.samples["ja"].rate,
            crate::config::DEFAULT_SAMPLE_RATE,
            "未配置 rate 的样本应使用默认语速"
        );
        assert_eq!(resolved.default_language, "zh");
        assert_eq!(mock.created.load(Ordering::SeqCst), 2, "两个语言各复刻一次");
        assert_eq!(cache.len(), 2);

        // 创建音色必须显式带参考音频时长上限（D30）
        let prompt_lengths: Vec<f64> = mock
            .server
            .requests()
            .iter()
            .filter_map(|request| {
                let body: serde_json::Value = serde_json::from_str(&request.body_text()).ok()?;
                if body["input"]["action"] != "create_voice" {
                    return None;
                }
                body["input"]["max_prompt_audio_length"].as_f64()
            })
            .collect();
        assert_eq!(
            prompt_lengths,
            vec![f64::from(MAX_PROMPT_AUDIO_LENGTH); 2],
            "每个音色创建请求都应带 max_prompt_audio_length"
        );

        // 再跑一次（重新读取缓存）：应全部命中，不再创建
        let mut cache = AppCache::load(&cache_path).unwrap();
        let resolved_again = resolve_custom_voice(&config, &api, &mut cache, name, custom, None)
            .await
            .unwrap();
        assert_eq!(resolved_again.samples, resolved.samples);
        assert_eq!(
            mock.created.load(Ordering::SeqCst),
            2,
            "缓存命中后不应重新创建音色"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn falls_back_to_cache_when_one_sample_file_is_missing() {
        let dir = temp_dir("missing-one-lang");
        let cache_path = dir.join("cache.json");
        let config = config_with_two_languages(&dir, &cache_path);
        let mock = mock_enrollment().await;
        let api =
            BailianVoiceApi::new("sk-test", format!("{}/api/v1", mock.server.base_url())).unwrap();
        let mut cache = AppCache::load(&cache_path).unwrap();
        let (name, custom) = config.active_custom_voice().unwrap();
        let first = resolve_custom_voice(&config, &api, &mut cache, name, custom, None)
            .await
            .unwrap();

        // 删掉中文样本文件后，应该靠缓存里的 audio_path 兜底继续使用
        std::fs::remove_file(dir.join("edgar-zh.wav")).ok();
        let mut cache = AppCache::load(&cache_path).unwrap();
        let second = resolve_custom_voice(&config, &api, &mut cache, name, custom, None)
            .await
            .unwrap();
        assert_eq!(
            second.samples.get("zh").map(|sample| &sample.voice_id),
            first.samples.get("zh").map(|sample| &sample.voice_id),
            "中文样本缺失时应复用缓存记录"
        );
        assert_eq!(
            second.samples.get("zh").map(|sample| sample.rate),
            first.samples.get("zh").map(|sample| sample.rate),
            "缓存兜底不应丢失该样本的语速"
        );
        assert_eq!(mock.created.load(Ordering::SeqCst), 2, "不应重复创建");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sample_prefix_is_valid_and_distinct_per_language() {
        assert_eq!(sample_prefix("edgar", Some("zh")), "edgarzh");
        assert_eq!(sample_prefix("edgar", Some("ja")), "edgarja");
        assert_eq!(sample_prefix("edgar", None), "edgar");
        // 派生结果必须满足百炼前缀规则：只含小写字母/数字、长度 1~10
        let is_valid = |prefix: &str| {
            !prefix.is_empty()
                && prefix.len() <= crate::config::MAX_VOICE_PREFIX_LEN
                && prefix
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        };
        assert!(is_valid(&sample_prefix(
            "a-very-long-prefix-name",
            Some("fil")
        )));
        // 超长键名时优先保留语言后缀，只截短键名部分
        assert_eq!(
            sample_prefix("a-very-long-prefix-name", Some("fil")),
            "averylofil"
        );
        assert_eq!(sample_prefix("abcdefghijklmn", Some("zh")), "abcdefghzh");
        // 超过官方上限时截断到 10
        let long = sample_prefix("abcdefghijklmn", None);
        assert!(is_valid(&long));
        assert_eq!(long.len(), crate::config::MAX_VOICE_PREFIX_LEN);
    }

    #[test]
    fn active_voice_falls_back_to_default_sample_language() {
        let mut samples = HashMap::new();
        samples.insert(
            "zh".to_string(),
            ResolvedSample {
                voice_id: "voice-zh".to_string(),
                rate: 1.3,
            },
        );
        samples.insert(
            "ja".to_string(),
            ResolvedSample {
                voice_id: "voice-ja".to_string(),
                rate: 1.0,
            },
        );
        let custom = ActiveVoice::Custom(ResolvedCustomVoice {
            display_name: "埃德嘉".to_string(),
            default_language: "zh".to_string(),
            samples,
        });
        assert_eq!(custom.selection_for("ja"), ("voice-ja", 1.0));
        assert_eq!(custom.selection_for("zh"), ("voice-zh", 1.3));
        // 没有对应语言样本（例如 en）时回退默认样本，语速也用默认样本的
        assert_eq!(custom.selection_for("en"), ("voice-zh", 1.3));
        assert_eq!(custom.voice_for("ja"), "voice-ja");

        let builtin = ActiveVoice::Builtin {
            name: "qiaoxiaojiao_v3.1".to_string(),
        };
        assert_eq!(
            builtin.selection_for("ja"),
            ("qiaoxiaojiao_v3.1", crate::config::DEFAULT_SAMPLE_RATE)
        );
        assert_eq!(builtin.voice_for("ja"), "qiaoxiaojiao_v3.1");
    }

    #[tokio::test]
    async fn oversized_audio_is_rejected_before_reading() {
        let dir = temp_dir("oversized");
        let cache_path = dir.join("cache.json");
        let audio = dir.join("huge.wav");
        std::fs::write(&audio, vec![0u8; MAX_AUDIO_BYTES + 1]).unwrap();
        let text = format!(
            "[api]\nkey = \"sk-test\"\n[tts]\nvoice = \"custom:big\"\n[cache]\nfile = \"{}\"\n\
             [tts.custom_voices.big]\n[[tts.custom_voices.big.samples]]\nlanguage = \"zh\"\naudio = \"{}\"\n",
            cache_path.display().to_string().replace('\\', "\\\\"),
            audio.display().to_string().replace('\\', "\\\\"),
        );
        let config = AppConfig::from_toml_str(&text).unwrap();
        let mock = mock_enrollment().await;
        let api =
            BailianVoiceApi::new("sk-test", format!("{}/api/v1", mock.server.base_url())).unwrap();
        let mut cache = AppCache::load(&cache_path).unwrap();
        let (name, custom) = config.active_custom_voice().unwrap();

        let err = resolve_custom_voice(&config, &api, &mut cache, name, custom, None)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("超过百炼上限 10 MB"), "{err:#}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
