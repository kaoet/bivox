//! `kaols voices` 子命令实现：列出 / 删除 / 清空百炼账号下的自定义音色。
//!
//! 删除支持三种模式（`kaols voices delete`）：
//! - `--voice-id <id>`：按百炼原生 voice_id 删除一个音色；
//! - `--voice <配置名>`：删除该音色配置下全部语言声线样本；
//! - `--voice <配置名> --language <code>`：只删除该音色的某个语言声线样本。

use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::collections::{BTreeMap, HashMap};
use std::io::{IsTerminal, Write};

use crate::config::AppConfig;
use crate::language;
use crate::voice::api;
use crate::voice::api::{BailianVoiceApi, RemoteVoice};
use crate::voice::cache::{self, VoiceCache};
use crate::voice::{api_from_config, cache_from_config, resolve_custom_voice};

/// 列出百炼账号下已上传的自定义音色，并交叉显示本地缓存。
pub async fn list(config: &AppConfig) -> Result<()> {
    let api = api_from_config(config)?;
    let mut cache = cache_from_config(config)?;
    list_with(&api, &mut cache).await
}

/// 删除音色（三种模式）。
pub async fn delete(
    config: &AppConfig,
    voice_id: Option<&str>,
    voice: Option<&str>,
    language_code: Option<&str>,
    yes: bool,
) -> Result<()> {
    match (voice_id, voice) {
        (Some(_), Some(_)) => bail!("--voice-id 与 --voice 不能同时使用"),
        (None, None) => {
            if language_code.is_some() {
                bail!("--language 只能与 --voice 一起使用");
            }
            bail!("请指定删除目标：--voice-id <id>，或 --voice <配置名> [--language <code>]")
        }
        (Some(voice_id), None) => {
            if language_code.is_some() {
                bail!("--language 只能与 --voice 一起使用");
            }
            let api = api_from_config(config)?;
            let mut cache = cache_from_config(config)?;
            delete_by_voice_id_with(&api, &mut cache, voice_id.trim()).await
        }
        (None, Some(voice)) => {
            let api = api_from_config(config)?;
            let mut cache = cache_from_config(config)?;
            delete_by_voice_config_with(config, &api, &mut cache, voice.trim(), language_code, yes)
                .await
        }
    }
}

/// 删除账号下全部自定义音色。
pub async fn delete_all(config: &AppConfig, yes: bool) -> Result<()> {
    let api = api_from_config(config)?;
    let mut cache = cache_from_config(config)?;
    delete_all_with(&api, &mut cache, yes).await
}

/// 立即上传并注册配置文件中的自定义音色（可指定音色名 / 语言）。
pub async fn enroll(
    config: &AppConfig,
    only: Option<&str>,
    only_language: Option<&str>,
) -> Result<()> {
    if config.tts.custom_voices.is_empty() {
        bail!(
            "配置文件中未定义任何自定义音色；请先添加 [tts.custom_voices.<名字>] 段与 [[...samples]]"
        );
    }

    let api = api_from_config(config)?;
    let mut cache = cache_from_config(config)?;

    let mut names: Vec<&str> = match only {
        Some(name) => {
            let name = name.trim();
            if !config.tts.custom_voices.contains_key(name) {
                let defined: Vec<&str> = config
                    .tts
                    .custom_voices
                    .keys()
                    .map(String::as_str)
                    .collect();
                bail!("配置文件中没有名为 {name:?} 的自定义音色（已定义: {defined:?}）");
            }
            vec![name]
        }
        None => config
            .tts
            .custom_voices
            .keys()
            .map(String::as_str)
            .collect(),
    };
    names.sort_unstable();

    println!(
        "{}",
        format!(
            "正在检查 {} 个自定义音色的百炼注册状态{}...",
            names.len(),
            only_language
                .map(|lang| format!("（只处理语言 {lang}）"))
                .unwrap_or_default()
        )
        .dimmed()
    );

    let mut failures = Vec::new();
    for name in names {
        let custom = &config.tts.custom_voices[name];
        println!("\n▶ 自定义音色「{}」", custom.display_name(name).cyan());
        match resolve_custom_voice(config, &api, &mut cache, name, custom, only_language).await {
            Ok(resolved) => println!(
                "  ✅ {} 已就绪，语言: {}",
                resolved.display_name,
                resolved.languages().join(", ")
            ),
            Err(e) => {
                eprintln!("  ❌ {e:#}");
                failures.push(name.to_string());
            }
        }
    }

    if !failures.is_empty() {
        bail!("{} 个自定义音色注册失败: {failures:?}", failures.len());
    }
    println!(
        "\n✅ 全部自定义音色均已就绪（缓存: {}）。",
        cache.path().display()
    );
    Ok(())
}

/// 使用给定 API 客户端列出音色（便于测试）。
async fn list_with(api: &BailianVoiceApi, cache: &mut VoiceCache) -> Result<()> {
    let list = api
        .list_voices()
        .await
        .context("获取百炼自定义音色列表失败")?;

    // 云端列表是权威来源：清理本地缓存里云端已不存在的记录（例如在控制台删除过）。
    // 只有确认列表已完整枚举时才清理，避免分页/响应异常时误删有效缓存。
    let mut pruned = Vec::new();
    if list.complete {
        pruned = cache.retain_voice_ids(|voice_id| {
            list.voices
                .iter()
                .any(|voice| !voice.voice_id.is_empty() && voice.voice_id == voice_id)
        });
        if !pruned.is_empty() {
            cache.save()?;
        }
    } else {
        println!(
            "⚠️ 未能确认百炼音色列表已完整枚举（已取到 {} 个，total={:?}），已跳过本地缓存清理",
            list.voices.len(),
            list.total_count
        );
    }

    print_voices(&list.voices, cache);

    if !pruned.is_empty() {
        println!(
            "🧹 已清理 {} 条云端不存在的本地缓存记录（缓存文件: {}）",
            pruned.len(),
            cache.path().display()
        );
    }
    if !cache.is_empty() {
        println!(
            "💾 本地缓存共 {} 条记录（{}）",
            cache.len(),
            cache.path().display()
        );
    }
    Ok(())
}

/// 删除单个百炼原生 voice_id（便于测试）。
async fn delete_by_voice_id_with(
    api: &BailianVoiceApi,
    cache: &mut VoiceCache,
    voice_id: &str,
) -> Result<()> {
    if voice_id.is_empty() {
        bail!("--voice-id 不能为空");
    }
    println!("将删除百炼音色: {}", voice_id.cyan());

    let deleted = match api.delete_voice(voice_id).await {
        Ok(()) => true,
        Err(e) if api::is_resource_missing_error(&e) => {
            println!("⚠️ 该音色在百炼账号中已不存在，仅同步本地缓存");
            false
        }
        Err(e) => return Err(e).with_context(|| format!("删除音色 {voice_id} 失败")),
    };

    let removed = cache.remove_by_voice_id(voice_id);
    if !removed.is_empty() {
        cache.save()?;
        println!("🧹 已同步移除本地缓存中 {} 条关联记录", removed.len());
    }
    if deleted {
        println!("{}", format!("✅ 已删除音色 {voice_id}").green());
    }
    Ok(())
}

/// 按配置名删除自定义音色的声线样本（便于测试）。
async fn delete_by_voice_config_with(
    config: &AppConfig,
    api: &BailianVoiceApi,
    cache: &mut VoiceCache,
    name: &str,
    only_language: Option<&str>,
    skip_confirm: bool,
) -> Result<()> {
    let custom = config.tts.custom_voices.get(name).with_context(|| {
        format!(
            "配置文件中没有 [tts.custom_voices.{name}]；\
             如需按百炼原生 voice_id 删除，请使用 --voice-id"
        )
    })?;
    let display_name = custom.display_name(name);

    let samples: Vec<_> = match only_language {
        Some(language_code) => {
            vec![custom.sample_for_language(language_code).with_context(|| {
                format!("自定义音色 [{name}] 没有语言 {language_code:?} 的声线样本")
            })?]
        }
        None => custom.samples.iter().collect(),
    };

    // 共享样例提示：同一个音频文件被其它音色配置使用时，删除会影响它们
    let selected_paths: Vec<String> = samples
        .iter()
        .map(|sample| config.resolve_path(&sample.audio).display().to_string())
        .collect();
    for (other_name, other) in &config.tts.custom_voices {
        if other_name == name {
            continue;
        }
        for sample in &other.samples {
            let path = config.resolve_path(&sample.audio).display().to_string();
            if selected_paths.contains(&path) {
                eprintln!(
                    "⚠️ 声线样本 {} 也被自定义音色 [{other_name}] 使用；删除后它下次启动会重新复刻",
                    path
                );
            }
        }
    }

    // voice_id -> (语言列表, 模型列表, 音频路径)
    let mut targets: BTreeMap<String, (Vec<String>, Vec<String>, String)> = BTreeMap::new();
    for sample in samples {
        let canonical = language::canonical_language(&sample.language).to_string();
        let audio_path = config.resolve_path(&sample.audio);
        let audio_path_text = audio_path.display().to_string();

        // 优先按音频内容哈希找（跨模型）；再按缓存里的 audio_path 兜底，
        // 覆盖「样例音频被替换后旧哈希仍留在缓存/云端」的情况。
        let hash = std::fs::read(&audio_path)
            .ok()
            .map(|bytes| cache::sha256_hex(&bytes));
        let mut entries: Vec<(&str, &cache::VoiceCacheEntry)> = Vec::new();
        if let Some(hash) = hash.as_deref() {
            entries.extend(cache.entries_for_hash(hash));
        }
        for (model, _, entry) in cache.entries() {
            if !entry.voice_id.is_empty() && entry.audio_path == audio_path_text {
                entries.push((model, entry));
            }
        }
        entries.sort_by(|a, b| (a.0, &a.1.voice_id).cmp(&(b.0, &b.1.voice_id)));
        entries.dedup_by(|a, b| a.0 == b.0 && a.1.voice_id == b.1.voice_id);

        if entries.is_empty() {
            println!(
                "⚠️ 自定义音色「{}」语言 {} 当前没有已注册音色（未注册或已删除），跳过\
                 （如需按原生 ID 删除请用 --voice-id）",
                display_name, canonical
            );
            continue;
        }
        for (model, entry) in entries {
            let target = targets.entry(entry.voice_id.clone()).or_default();
            if !target.0.contains(&canonical) {
                target.0.push(canonical.clone());
            }
            if !target.1.iter().any(|m| m == model) {
                target.1.push(model.to_string());
            }
            target.2 = audio_path_text.clone();
        }
    }

    if targets.is_empty() {
        bail!("没有找到可删除的音色（对应样本尚未注册，或本地缓存缺失）");
    }

    println!(
        "\n将删除自定义音色「{}」的 {} 个声线:",
        display_name.cyan(),
        targets.len()
    );
    for (voice_id, (languages, models, audio_path)) in &targets {
        println!(
            "  - {}  语言 {}  模型 {}  音频 {}",
            voice_id,
            languages.join(","),
            models.join(","),
            audio_path
        );
    }

    if targets.len() > 1
        && !skip_confirm
        && !confirm(&format!(
            "即将删除 {} 个 voice_id，是否继续？",
            targets.len()
        ))?
    {
        println!("❌ 操作已取消。");
        return Ok(());
    }

    let mut removed = 0usize;
    let mut failed = Vec::new();
    for voice_id in targets.keys() {
        match api.delete_voice(voice_id).await {
            Ok(()) => {
                println!("{}", format!("✅ 已删除 {voice_id}").green());
                removed += cache.remove_by_voice_id(voice_id).len();
            }
            Err(e) if api::is_resource_missing_error(&e) => {
                println!("⚠️ {voice_id} 在百炼账号中已不存在，仅同步本地缓存");
                removed += cache.remove_by_voice_id(voice_id).len();
            }
            Err(e) => {
                println!("❌ 删除 {voice_id} 失败: {e:#}");
                failed.push(voice_id.clone());
            }
        }
    }
    if removed > 0 {
        cache.save()?;
        println!("🧹 已同步移除本地缓存中 {removed} 条关联记录");
    }
    if !failed.is_empty() {
        bail!("{} 个音色删除失败，详见上方输出", failed.len());
    }
    Ok(())
}

/// 使用给定 API 客户端删除全部音色（便于测试）。
async fn delete_all_with(
    api: &BailianVoiceApi,
    cache: &mut VoiceCache,
    skip_confirm: bool,
) -> Result<()> {
    let list = api
        .list_voices()
        .await
        .context("获取百炼自定义音色列表失败")?;
    if !list.complete {
        bail!(
            "无法获取完整的音色列表（分页未完成，已取到 {} 个，total={:?}）；\
             为避免误判「删除全部」的结果，请稍后重试",
            list.voices.len(),
            list.total_count
        );
    }
    if list.voices.is_empty() {
        println!("📭 百炼账号下当前没有自定义音色，无需删除。");
        return Ok(());
    }

    let voices = list.voices;
    print_voices(&voices, cache);
    if !skip_confirm
        && !confirm(&format!(
            "即将删除全部 {} 个自定义音色，是否继续？",
            voices.len()
        ))?
    {
        println!("❌ 操作已取消。");
        return Ok(());
    }

    let mut deleted = Vec::new();
    let mut failed = Vec::new();
    for (index, voice) in voices.iter().enumerate() {
        print!(
            "[{}/{}] 正在删除 {} ... ",
            index + 1,
            voices.len(),
            voice.voice_id
        );
        let _ = std::io::stdout().flush();

        match api.delete_voice(&voice.voice_id).await {
            Ok(()) => {
                println!("✅");
                deleted.push(voice.voice_id.clone());
            }
            Err(e) if api::is_resource_missing_error(&e) => {
                println!("⚠️ 已不存在，同步缓存");
                deleted.push(voice.voice_id.clone());
            }
            Err(e) => {
                println!("❌ {e:#}");
                failed.push(voice.voice_id.clone());
            }
        }
    }

    if !deleted.is_empty() {
        if failed.is_empty() {
            cache.clear();
        } else {
            for voice_id in &deleted {
                cache.remove_by_voice_id(voice_id);
            }
        }
        cache.save()?;
    }

    println!(
        "\n🎉 批量删除完成: 成功 {} 个，失败 {} 个。",
        deleted.len(),
        failed.len()
    );
    if !failed.is_empty() {
        bail!("{} 个音色删除失败，详见上方输出", failed.len());
    }
    Ok(())
}

/// 交互式确认；返回是否确认执行。
fn confirm(prompt: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("当前不是交互式终端，无法确认；请加 -y/--yes 跳过确认");
    }
    println!("\n{}", format!("⚠️ {prompt}").red());
    print!("请输入 YES 确认: ");
    std::io::stdout().flush().context("刷新终端输出失败")?;

    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("读取确认输入失败")?;
    Ok(line.trim() == "YES")
}

/// 打印音色表格，并标注每个音色的本地缓存关联。
fn print_voices(voices: &[RemoteVoice], cache: &VoiceCache) {
    if voices.is_empty() {
        println!("\n📭 百炼账号下当前没有自定义音色。\n");
        return;
    }

    // voice_id -> 本地缓存的音色名/语言/音频路径（同一音色可能缓存在多个模型下，去重展示）
    let mut local_notes: HashMap<&str, Vec<String>> = HashMap::new();
    for (_, _, entry) in cache.entries() {
        let language_tag = if entry.language.is_empty() {
            String::new()
        } else {
            format!(" [{}]", entry.language)
        };
        let note = if entry.audio_path.is_empty() {
            format!("{}{}", entry.name, language_tag)
        } else {
            format!("{}{}（{}）", entry.name, language_tag, entry.audio_path)
        };
        let notes = local_notes.entry(entry.voice_id.as_str()).or_default();
        if !notes.contains(&note) {
            notes.push(note);
        }
    }

    let id_width = voices
        .iter()
        .map(|voice| voice.voice_id.chars().count())
        .max()
        .unwrap_or(8)
        .max("Voice ID".len());

    println!(
        "\n{}",
        format!("=== 百炼自定义音色（共 {} 个） ===", voices.len()).bold()
    );
    println!(
        "{:<4} {:<id_width$} {:<8} 本地关联",
        "序号",
        "Voice ID",
        "状态",
        id_width = id_width,
    );

    for (index, voice) in voices.iter().enumerate() {
        let status = format!("{:<8}", voice.status);
        let status = if voice.is_ready() {
            status.green()
        } else {
            status.yellow()
        };
        let local = local_notes
            .get(voice.voice_id.as_str())
            .map(|notes| notes.join("; "))
            .unwrap_or_else(|| "—".to_string());
        println!(
            "{:<4} {:<id_width$} {} {}",
            format!("[{}]", index + 1),
            voice.voice_id,
            status,
            local,
            id_width = id_width,
        );
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::cache::VoiceCacheEntry;
    use crate::voice::mock_http::{MockHttpServer, MockResponse, RecordedRequest};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    const TEST_MODEL: &str = "qwen-audio-3.1-tts-flash";

    fn temp_cache_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "kaols-voice-cli-{tag}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn remote(voice_id: &str, status: &str) -> RemoteVoice {
        RemoteVoice {
            voice_id: voice_id.to_string(),
            target_model: TEST_MODEL.to_string(),
            status: status.to_string(),
            created_time: String::new(),
        }
    }

    fn cache_entry(voice_id: &str, language_code: &str) -> VoiceCacheEntry {
        VoiceCacheEntry {
            voice_id: voice_id.to_string(),
            language: language_code.to_string(),
            name: "Kaoet".to_string(),
            audio_path: "samples/kaoet.wav".to_string(),
            created_at: 0,
        }
    }

    /// 简化测试里「在测试模型下插入一条缓存」的写法。
    fn insert(cache: &mut VoiceCache, audio_hash: &str, voice_id: &str, language_code: &str) {
        cache.insert(TEST_MODEL, audio_hash, cache_entry(voice_id, language_code));
    }

    fn list_response(voices: &[RemoteVoice]) -> MockResponse {
        let items: Vec<String> = voices
            .iter()
            .map(|voice| {
                format!(
                    r#"{{"voice_id":"{}","target_model":"{}","status":"{}"}}"#,
                    voice.voice_id, voice.target_model, voice.status
                )
            })
            .collect();
        MockResponse::json(format!(
            r#"{{"output":{{"page_index":0,"page_size":50,"total_count":{},"voice_list":[{}]}}}}"#,
            voices.len(),
            items.join(",")
        ))
    }

    fn handler_for(
        voices: Vec<RemoteVoice>,
    ) -> impl Fn(&RecordedRequest) -> MockResponse + Send + Sync + 'static {
        let deleted = Arc::new(AtomicUsize::new(0));
        move |request: &RecordedRequest| {
            if request.method == "POST"
                && request.path == "/api/v1/services/audio/tts/customization"
            {
                let body: serde_json::Value =
                    serde_json::from_str(&request.body_text()).unwrap_or_default();
                match body["input"]["action"].as_str() {
                    Some("list_voice") => list_response(&voices),
                    Some("delete_voice") => {
                        deleted.fetch_add(1, Ordering::SeqCst);
                        MockResponse::json(r#"{"output":{}}"#)
                    }
                    _ => MockResponse::json(r#"{"code":"Unknown","message":"unexpected"}"#)
                        .status(400),
                }
            } else {
                MockResponse::json(r#"{"code":"NotFound","message":"unexpected"}"#).status(404)
            }
        }
    }

    async fn api_for(voices: Vec<RemoteVoice>) -> (BailianVoiceApi, MockHttpServer) {
        let server = MockHttpServer::start(handler_for(voices)).await;
        let api = BailianVoiceApi::new("sk-test", format!("{}/api/v1", server.base_url())).unwrap();
        (api, server)
    }

    fn config_with_voice(
        tag: &str,
        samples: &[(&str, &str)],
        cache_path: &Path,
    ) -> (AppConfig, Vec<PathBuf>) {
        let dir = std::env::temp_dir().join(format!(
            "kaols-voice-config-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let mut sample_toml = String::new();
        let mut audio_paths = Vec::new();
        for (language_code, content) in samples {
            let audio_path = dir.join(format!("sample-{language_code}.wav"));
            std::fs::write(&audio_path, content.as_bytes()).unwrap();
            audio_paths.push(audio_path.clone());
            sample_toml.push_str(&format!(
                "\n[[tts.custom_voices.edgar.samples]]\nlanguage = \"{language_code}\"\naudio = \"{}\"\n",
                audio_path.display().to_string().replace('\\', "\\\\")
            ));
        }
        let text = format!(
            "[api]\nkey = \"sk-test\"\n[tts]\nvoice = \"custom:edgar\"\ncache_file = \"{}\"\n\
             [tts.custom_voices.edgar]\nname = \"埃德嘉\"\n{sample_toml}",
            cache_path.display().to_string().replace('\\', "\\\\")
        );
        (AppConfig::from_toml_str(&text).unwrap(), audio_paths)
    }

    #[tokio::test]
    async fn delete_by_voice_id_removes_voice_and_cache_mapping() {
        let (api, server) = api_for(vec![]).await;
        let cache_path = temp_cache_path("delete-id");
        let mut cache = VoiceCache::load(&cache_path).unwrap();
        insert(&mut cache, "hash-a1", "voice-a", "zh");
        insert(&mut cache, "hash-a2", "voice-a", "ja");
        insert(&mut cache, "hash-b", "voice-b", "zh");

        delete_by_voice_id_with(&api, &mut cache, "voice-a")
            .await
            .unwrap();

        let persisted = VoiceCache::load(&cache_path).unwrap();
        assert!(persisted.get(TEST_MODEL, "hash-a1").is_none());
        assert!(persisted.get(TEST_MODEL, "hash-a2").is_none());
        assert!(persisted.get(TEST_MODEL, "hash-b").is_some());

        let delete_requests = server
            .requests()
            .iter()
            .filter(|request| request.body_text().contains("\"action\":\"delete_voice\""))
            .count();
        assert_eq!(delete_requests, 1);

        std::fs::remove_file(&cache_path).ok();
    }

    #[tokio::test]
    async fn delete_by_config_voice_removes_all_language_samples() {
        let (api, server) = api_for(vec![]).await;
        let cache_path = temp_cache_path("delete-config");
        let (config, audio_paths) = config_with_voice(
            "all",
            &[("zh", "zh-content"), ("ja", "ja-content")],
            &cache_path,
        );

        let mut cache = VoiceCache::load(&cache_path).unwrap();
        let zh_hash = cache::sha256_hex(&std::fs::read(&audio_paths[0]).unwrap());
        let ja_hash = cache::sha256_hex(&std::fs::read(&audio_paths[1]).unwrap());
        insert(&mut cache, &zh_hash, "voice-zh", "zh");
        insert(&mut cache, &ja_hash, "voice-ja", "ja");
        cache.save().unwrap();

        delete_by_voice_config_with(&config, &api, &mut cache, "edgar", None, true)
            .await
            .unwrap();

        let persisted = VoiceCache::load(&cache_path).unwrap();
        assert!(persisted.is_empty(), "两种语言的缓存都应被移除");
        let delete_requests = server
            .requests()
            .iter()
            .filter(|request| request.body_text().contains("\"action\":\"delete_voice\""))
            .count();
        assert_eq!(delete_requests, 2);

        std::fs::remove_dir_all(audio_paths[0].parent().unwrap()).ok();
        std::fs::remove_file(&cache_path).ok();
    }

    #[tokio::test]
    async fn delete_by_config_voice_and_language_removes_only_that_language() {
        let (api, server) = api_for(vec![]).await;
        let cache_path = temp_cache_path("delete-lang");
        let (config, audio_paths) = config_with_voice(
            "lang",
            &[("zh", "zh-content"), ("ja", "ja-content")],
            &cache_path,
        );

        let mut cache = VoiceCache::load(&cache_path).unwrap();
        let zh_hash = cache::sha256_hex(&std::fs::read(&audio_paths[0]).unwrap());
        let ja_hash = cache::sha256_hex(&std::fs::read(&audio_paths[1]).unwrap());
        insert(&mut cache, &zh_hash, "voice-zh", "zh");
        insert(&mut cache, &ja_hash, "voice-ja", "ja");
        cache.save().unwrap();

        delete_by_voice_config_with(&config, &api, &mut cache, "edgar", Some("ja"), true)
            .await
            .unwrap();

        let persisted = VoiceCache::load(&cache_path).unwrap();
        assert!(
            persisted.get(TEST_MODEL, &zh_hash).is_some(),
            "中文声线应保留"
        );
        assert!(
            persisted.get(TEST_MODEL, &ja_hash).is_none(),
            "日文声线应删除"
        );
        let delete_requests = server
            .requests()
            .iter()
            .filter(|request| request.body_text().contains("\"action\":\"delete_voice\""))
            .count();
        assert_eq!(delete_requests, 1);

        std::fs::remove_dir_all(audio_paths[0].parent().unwrap()).ok();
        std::fs::remove_file(&cache_path).ok();
    }

    /// 样例音频被替换（哈希变化）后，仍应按缓存里的 audio_path 找到旧音色。
    #[tokio::test]
    async fn delete_by_config_finds_voice_by_path_after_audio_replaced() {
        let (api, server) = api_for(vec![]).await;
        let cache_path = temp_cache_path("delete-replaced");
        let (config, audio_paths) =
            config_with_voice("replaced", &[("zh", "old-content")], &cache_path);

        let mut cache = VoiceCache::load(&cache_path).unwrap();
        let old_hash = cache::sha256_hex(b"old-content");
        let mut entry = cache_entry("voice-old", "zh");
        entry.audio_path = audio_paths[0].display().to_string();
        cache.insert(TEST_MODEL, &old_hash, entry);
        cache.save().unwrap();

        // 替换样例音频：新内容哈希与旧缓存不同
        std::fs::write(&audio_paths[0], b"new-content").unwrap();

        delete_by_voice_config_with(&config, &api, &mut cache, "edgar", None, true)
            .await
            .unwrap();

        let persisted = VoiceCache::load(&cache_path).unwrap();
        assert!(persisted.is_empty(), "旧哈希记录也应被删除");
        let delete_requests = server
            .requests()
            .iter()
            .filter(|request| request.body_text().contains("\"action\":\"delete_voice\""))
            .count();
        assert_eq!(delete_requests, 1);

        std::fs::remove_dir_all(audio_paths[0].parent().unwrap()).ok();
        std::fs::remove_file(&cache_path).ok();
    }

    #[test]
    fn delete_requires_exactly_one_target_mode() {
        let config = AppConfig::from_toml_str("[api]\nkey = \"sk-test\"\n").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let neither = runtime.block_on(delete(&config, None, None, None, true));
        assert!(format!("{:#}", neither.unwrap_err()).contains("--voice-id"));

        let both = runtime.block_on(delete(&config, Some("voice-1"), Some("edgar"), None, true));
        assert!(format!("{:#}", both.unwrap_err()).contains("不能同时使用"));

        let language_without_voice =
            runtime.block_on(delete(&config, Some("voice-1"), None, Some("zh"), true));
        assert!(format!("{:#}", language_without_voice.unwrap_err()).contains("--language"));
    }

    #[tokio::test]
    async fn list_with_prunes_stale_cache_entries() {
        let (api, _server) = api_for(vec![remote("voice-a", "OK")]).await;

        let cache_path = temp_cache_path("list-prune");
        let mut cache = VoiceCache::load(&cache_path).unwrap();
        insert(&mut cache, "hash-a", "voice-a", "zh");
        insert(&mut cache, "hash-gone", "voice-deleted-in-console", "zh");

        list_with(&api, &mut cache).await.unwrap();

        let persisted = VoiceCache::load(&cache_path).unwrap();
        assert!(persisted.get(TEST_MODEL, "hash-a").is_some());
        assert!(
            persisted.get(TEST_MODEL, "hash-gone").is_none(),
            "云端不存在的缓存记录应被清理"
        );

        std::fs::remove_file(&cache_path).ok();
    }

    /// 云列表未能确认完整时，绝不能据此清理本地缓存。
    #[tokio::test]
    async fn list_with_skips_prune_when_cloud_list_is_incomplete() {
        let server = MockHttpServer::start(|_request| {
            // 异常 200：没有 voice_list 字段
            MockResponse::json(r#"{"output":{"page_index":0,"page_size":50}}"#)
        })
        .await;
        let api = BailianVoiceApi::new("sk-test", format!("{}/api/v1", server.base_url())).unwrap();

        let cache_path = temp_cache_path("list-incomplete");
        let mut cache = VoiceCache::load(&cache_path).unwrap();
        insert(&mut cache, "hash-a", "voice-a", "zh");
        cache.save().unwrap();

        list_with(&api, &mut cache).await.unwrap();

        let persisted = VoiceCache::load(&cache_path).unwrap();
        assert!(
            persisted.get(TEST_MODEL, "hash-a").is_some(),
            "列表不完整时不应清理任何缓存"
        );

        std::fs::remove_file(&cache_path).ok();
    }

    #[tokio::test]
    async fn delete_all_with_yes_clears_all_voices_and_cache() {
        let (api, server) = api_for(vec![remote("voice-a", "OK"), remote("voice-b", "OK")]).await;

        let cache_path = temp_cache_path("delete-all");
        let mut cache = VoiceCache::load(&cache_path).unwrap();
        insert(&mut cache, "hash-a", "voice-a", "zh");
        insert(&mut cache, "hash-b", "voice-b", "ja");
        cache.save().unwrap();

        delete_all_with(&api, &mut cache, true).await.unwrap();

        let persisted = VoiceCache::load(&cache_path).unwrap();
        assert!(persisted.is_empty(), "全部删除后缓存应清空");

        let delete_requests = server
            .requests()
            .iter()
            .filter(|request| request.body_text().contains("\"action\":\"delete_voice\""))
            .count();
        assert_eq!(delete_requests, 2);

        std::fs::remove_file(&cache_path).ok();
    }

    #[tokio::test]
    async fn delete_all_with_empty_account_is_noop() {
        let (api, _server) = api_for(vec![]).await;
        let cache_path = temp_cache_path("empty");
        let mut cache = VoiceCache::load(&cache_path).unwrap();

        delete_all_with(&api, &mut cache, true).await.unwrap();
        assert!(cache.is_empty());
    }

    /// 列表不完整（即使看起来为空）时，delete-all 必须拒绝而不是假装"没有音色"。
    #[tokio::test]
    async fn delete_all_with_incomplete_list_is_rejected() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(r#"{"output":{"page_index":0,"page_size":50}}"#)
        })
        .await;
        let api = BailianVoiceApi::new("sk-test", format!("{}/api/v1", server.base_url())).unwrap();

        let cache_path = temp_cache_path("delete-all-incomplete");
        let mut cache = VoiceCache::load(&cache_path).unwrap();
        insert(&mut cache, "hash-a", "voice-a", "zh");
        cache.save().unwrap();

        let err = delete_all_with(&api, &mut cache, true).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("无法获取完整的音色列表"),
            "{err:#}"
        );

        let persisted = VoiceCache::load(&cache_path).unwrap();
        assert!(
            persisted.get(TEST_MODEL, "hash-a").is_some(),
            "拒绝执行时不应清理缓存"
        );
        std::fs::remove_file(&cache_path).ok();
    }
}
