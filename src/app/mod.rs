//! GUI 应用主体：启动准备、全局状态、面板布局、事件泵、退出流程。

mod drafts;

use std::collections::{BTreeMap, VecDeque};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use eframe::egui;
use log::{debug, error, info, warn};
use tokio::sync::mpsc;

use crate::audio::device::AudioDeviceManager;
use crate::audio::meter::MeterSmoother;
use crate::config::{
    AppConfig, CUSTOM_VOICE_PREFIX, DEFAULT_VOICE, TranslateGranularity, VrchatDisplay,
};
use crate::config_edit;
use crate::events::{OthersLineState, OthersStatus, PipelineEvent, SessionState, Speaker};
use crate::gui;
use crate::log_sink::{self, LogLine, MAX_LOG_LINES};
use crate::osc::{ChatboxService, ChatboxSettings};
use crate::paths::AppPaths;
use crate::pipeline::session::{MAX_TYPED_INPUT_CHARS, PipelineSessionHandle, TypedInput};
use crate::timeline::Timeline;
use crate::voice::editor;

/// 窗口标题。
pub const WINDOW_TITLE: &str = "Bivox — 实时语音识别与合成";

pub use drafts::Drafts;
use drafts::{AsrDraft, LocalDraft, OthersDraft, SettingsDraft, TtsDraft, VadDraft};

/// 语音合成页顶部「当前音色」的选择方式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum VoiceMode {
    /// 现有音色：内置音色名或任意 voice_id（输入框）。
    #[default]
    Existing,
    /// 自定义音色：从 `[tts.custom_voices]` 里选（下拉菜单），写 `custom:<键名>`。
    Custom,
}

/// 页签。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tab {
    Realtime,
    Audio,
    /// 「自己」页：我方线路的专属设置。
    Local,
    /// 「他人」页：他人线路的专属设置。
    Others,
    Asr,
    Tts,
    Vrchat,
    Translate,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 9] = [
        Tab::Realtime,
        Tab::Audio,
        Tab::Local,
        Tab::Others,
        Tab::Asr,
        Tab::Tts,
        Tab::Vrchat,
        Tab::Translate,
        Tab::Settings,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Realtime => "实时",
            Self::Audio => "音频",
            Self::Local => "自己",
            Self::Others => "他人",
            Self::Asr => "语音识别",
            Self::Tts => "语音合成",
            Self::Vrchat => "VRChat",
            Self::Translate => "翻译",
            Self::Settings => "设置",
        }
    }
}

/// 设备名去空白；空字符串等价于系统默认。
fn normalize_device(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 应用状态。
pub struct BivoxApp {
    pub paths: AppPaths,
    /// 后台运行时：用它 spawn `PipelineSession`。
    pub runtime: tokio::runtime::Runtime,
    /// VRChat OSC 聊天框后台服务。
    pub chatbox: ChatboxService,
    pub tab: Tab,
    pub state: SessionState,
    /// 正在运行的实时会话（`None` = 未启动）。
    pub session: Option<PipelineSessionHandle>,
    /// 后台事件接收端（与 `session` 同生命周期）。
    pub events_rx: Option<mpsc::UnboundedReceiver<PipelineEvent>>,
    /// 启动会话时使用的配置指纹（用于判断「运行中配置被改」）。
    pub running_config_signature: Option<String>,
    /// 配置变更后等待旧会话退出、再自动重启。
    pub restart_pending: bool,
    /// 最近一帧的 egui Context（自动重启时用来请求重绘）。
    pub egui_ctx: Option<egui::Context>,
    /// 当前有效配置；启动时加载失败会直接退出，因此这里始终是合法配置。
    pub config: AppConfig,
    /// 各页签的未保存草稿（状态与 `sync/dirty/apply` 都在 [`drafts`] 里）。
    pub drafts: Drafts,
    /// 语音合成页左栏当前选中的自定义音色（配置键名）；`None` = 未选中。
    ///
    /// 只是界面选中态，不是配置草稿，所以不放进 [`Drafts`]。
    pub selected_voice: Option<String>,
    /// 启动实时会话时发现未配置 API Key 的提示（设置页顶部显示）。
    pub api_key_notice: Option<String>,
    /// 实时页底部「打字输入」的输入框内容。
    pub typed_input: String,
    /// 打字输入的行内错误（超长、会话未开始等）。
    pub typed_input_error: Option<String>,
    /// 设备下拉的可选项（启动时与「刷新设备」时重新枚举）。
    pub input_devices: Vec<String>,
    pub output_devices: Vec<String>,
    /// 页签级的保存/校验错误（页内红字）；同一页签最多一条。
    pub errors: BTreeMap<Tab, String>,
    /// 不归属任何页签的写回错误（顶栏横幅 + 「清除错误」）。
    ///
    /// 目前只有顶栏快捷开关（麦克风/他人侦听）写回失败走这里：它不属于任何一个页签。
    pub config_write_error: Option<String>,
    /// 运行中重新加载配置失败的原因（保留旧配置；成功时清空）。
    pub config_reload_error: Option<String>,
    /// 实时对话时间线（两条线路的消息：最新在前，未完成消息按进度分色）。
    pub timeline: Timeline,
    /// 音色列表在下一次绘制时滚到底部（新增音色后使用，滚完即清）。
    pub voice_list_scroll_to_end: bool,
    /// 运行期告警（顶部黄色横幅显示，可手动清除）。
    pub pipeline_warning: Option<String>,
    /// 实时管线报错（顶部横幅显示，可手动清除）。
    pub pipeline_error: Option<String>,
    /// 本地 VAD 当前是否处于语音段（顶栏电平条配色用）。
    pub vad_active: bool,
    /// 他人线路的 VAD 指示（由对方线路上报；对方线路未启用时保持 false）。
    pub vad_active_others: bool,
    /// 顶栏麦克风电平（平滑后，0..1）。
    pub mic_level: f32,
    /// 顶栏他人线路电平（平滑后，0..1）。
    pub others_level: f32,
    /// 两条电平条的平滑器。
    pub mic_meter: MeterSmoother,
    pub others_meter: MeterSmoother,
    /// 他人线路的最近状态（`None` = 未启用或会话未运行）。
    pub others_status: Option<OthersStatus>,
    /// 他人线路下一次重试的相对时刻（由 `retry_in` 换算，界面倒计时用）。
    pub others_retry_deadline: Option<Instant>,
    /// 启动检查发现的问题（日志初始化失败等非致命问题）。
    pub startup_error: Option<String>,
    /// 运行期崩溃信息（panic 钩子写入）。
    pub crash: Option<String>,
    pub log: VecDeque<LogLine>,
    /// 已经收到关闭请求、正在收尾。
    pub closing: bool,
}

impl BivoxApp {
    fn new(
        paths: AppPaths,
        runtime: tokio::runtime::Runtime,
        config: AppConfig,
        startup_error: Option<String>,
    ) -> Self {
        // 配置已经在 app::run 里加载并校验过；无效配置不会走到 GUI。
        let tab = Tab::Realtime;
        let chatbox =
            ChatboxService::spawn(runtime.handle(), ChatboxSettings::from_config(&config));

        let mut app = Self {
            paths,
            runtime,
            chatbox,
            tab,
            state: SessionState::Idle,
            session: None,
            events_rx: None,
            running_config_signature: None,
            restart_pending: false,
            egui_ctx: None,
            config,
            drafts: Drafts::default(),
            selected_voice: None,
            api_key_notice: None,
            typed_input: String::new(),
            typed_input_error: None,
            input_devices: Vec::new(),
            output_devices: Vec::new(),
            errors: BTreeMap::new(),
            config_write_error: None,
            config_reload_error: None,
            timeline: Timeline::default(),
            voice_list_scroll_to_end: false,
            pipeline_warning: None,
            pipeline_error: None,
            vad_active: false,
            vad_active_others: false,
            mic_level: 0.0,
            others_level: 0.0,
            mic_meter: MeterSmoother::default(),
            others_meter: MeterSmoother::default(),
            others_status: None,
            others_retry_deadline: None,
            startup_error,
            crash: None,
            log: VecDeque::new(),
            closing: false,
        };
        app.sync_from_config();
        app.refresh_devices();
        app
    }

    /// 重新从磁盘读取配置（设置页「重新加载」；写回成功后也会调用）。
    ///
    /// 失败时保留当前内存配置并记录错误，不把 GUI 切到“不可用”状态；
    /// 写回路径也会重新校验磁盘文件，所以此时不会覆盖非法配置。
    pub fn reload_config(&mut self) {
        match AppConfig::load(&self.paths.config_file) {
            Ok(config) => {
                self.config_reload_error = None;
                // 重新加载成功 = 磁盘配置已合法，之前各页签的保存错误都已过期
                self.errors.clear();
                self.config_write_error = None;
                self.apply_config(config);
                info!("已重新加载配置：{}", self.paths.config_file.display());
            }
            Err(err) => {
                let message = format!("{err:#}");
                self.config_reload_error = Some(message.clone());
                warn!(
                    "重新加载配置失败，保留当前配置：{message}（{}）",
                    self.paths.config_file.display()
                );
            }
        }
    }

    /// 应用一份已校验的配置，并刷新依赖它的界面状态。
    fn apply_config(&mut self, config: AppConfig) {
        self.config = config;
        if self.config.api.key.is_some() {
            self.api_key_notice = None;
        }
        self.sync_from_config();
        // 选中的音色若仍存在就保留（保存后不丢选择），否则清空
        let selected_exists = self
            .selected_voice
            .as_ref()
            .is_some_and(|key| self.drafts.tts.drafts.iter().any(|draft| &draft.key == key));
        if !selected_exists {
            self.selected_voice = None;
        }
        // 空闲时让时间线的翻译模式立刻跟随配置（运行中的会话等 D20 重启后由 start_session 设置）。
        if self.session.is_none() {
            self.apply_translation_modes();
        }
        // 运行中改配置（Key/设备/音色等）→ 用新配置自动重启管线
        self.maybe_restart_session();
    }

    /// 把配置同步到各页签草稿（启动、重载、写回成功后调用）。
    ///
    /// 逐字段的映射在 [`Drafts::from_config`] 里；这里只处理不属于草稿的旁路状态。
    fn sync_from_config(&mut self) {
        self.drafts = Drafts::from_config(&self.config, &self.drafts);
        self.chatbox
            .update_settings(ChatboxSettings::from_config(&self.config));
    }

    /// 语音合成页是否有未保存改动（模型 + 音色草稿 + 当前音色 + 中文方言）。
    pub fn tts_tab_dirty(&self) -> bool {
        self.drafts.tts.is_dirty(&self.config)
    }

    /// 保存语音合成页的全部改动：合成模型、`[tts.custom_voices]` 草稿、当前音色与中文方言。
    ///
    /// 四部分合并成一次原子写回（一次校验 + 一份 `.bak`），成功后应用配置并按 D20 热重启。
    pub fn save_tts_tab(&mut self) {
        if !self.tts_tab_dirty() {
            return;
        }
        match self.try_save_tts_tab() {
            Ok(config) => {
                self.errors.remove(&Tab::Tts);
                self.apply_config(config);
                info!("已保存语音合成设置（{}）", self.paths.config_file.display());
            }
            Err(err) => self.fail_tab(Tab::Tts, err, "保存语音合成设置失败"),
        }
    }

    fn try_save_tts_tab(&self) -> anyhow::Result<AppConfig> {
        let tts = &self.drafts.tts;
        let model_dirty = tts.model_dirty(&self.config);
        let drafts_dirty = tts.voices_dirty(&self.config);
        let voice_dirty = tts.selection_dirty(&self.config);
        let dialect_dirty = tts.dialect_dirty(&self.config);
        let wanted_voice = tts.selection();

        config_edit::update(&self.paths.config_file, |config| {
            if model_dirty {
                let model = tts.model.trim();
                if model.is_empty() {
                    anyhow::bail!("合成模型不能为空");
                }
                config.tts.model = model.to_string();
            }

            if drafts_dirty {
                let entries = editor::drafts_to_entries(&tts.drafts)?;
                // 样例音频必须存在（相对路径按配置文件目录解析）
                for (key, voice) in &entries {
                    for sample in &voice.samples {
                        let path = config.resolve_path(&sample.audio);
                        if !path.is_file() {
                            anyhow::bail!("音色 `{key}` 的样例音频不存在：{}", path.display());
                        }
                    }
                }

                // 没有显式改当前音色时：生效自定义音色被删除/改名 → 自动回退
                let current = config.custom_voice_name().map(str::to_string);
                let fallback = if voice_dirty {
                    None
                } else {
                    current.and_then(|current| {
                        if entries.iter().any(|(key, _)| key == &current) {
                            None
                        } else {
                            Some(
                                entries
                                    .first()
                                    .map(|(first, _)| format!("{CUSTOM_VOICE_PREFIX}{first}"))
                                    .unwrap_or_else(|| DEFAULT_VOICE.to_string()),
                            )
                        }
                    })
                };

                config.tts.custom_voices = entries.into_iter().collect();
                if let Some(fallback) = fallback {
                    config.tts.voice = fallback;
                }
            }

            if voice_dirty {
                if wanted_voice.is_empty() {
                    anyhow::bail!("当前音色不能为空");
                }
                config.tts.voice = wanted_voice;
            }

            if dialect_dirty {
                config.tts.dialect = self.drafts.tts.dialect.trim().to_string();
            }
            Ok(())
        })
    }

    /// 放弃语音合成页的全部改动：自定义音色草稿 + 顶部「当前音色」。
    pub fn reset_tts_tab(&mut self) {
        // 放弃修改后按行号恢复左侧选中项（与编辑器里的排序/改名无关）
        let wanted_index = self
            .selected_voice
            .as_ref()
            .and_then(|key| self.drafts.tts.drafts.iter().position(|d| &d.key == key));
        self.drafts.tts = TtsDraft::from_config(&self.config, &self.drafts.tts);
        self.errors.remove(&Tab::Tts);
        if let Some(index) = wanted_index
            && let Some(draft) = self.drafts.tts.drafts.get(index)
        {
            self.selected_voice = Some(draft.key.clone());
        }
    }

    // ---- 语音识别页 ----

    /// 语音识别页是否有未保存改动。
    pub fn asr_tab_dirty(&self) -> bool {
        self.drafts.asr.is_dirty(&self.config)
    }

    /// 保存语音识别页的全部改动（模型 + 热词，一次原子写回）。
    ///
    /// 成功后应用配置；运行中会按 D20 自动重启实时管线，下次会话启动时自动
    /// 创建/更新百炼预编译热词列表（D31）。
    pub fn save_asr_tab(&mut self) {
        if !self.asr_tab_dirty() {
            return;
        }
        match self.try_save_asr_tab() {
            Ok(config) => {
                self.errors.remove(&Tab::Asr);
                self.apply_config(config);
                info!("已保存语音识别设置（{}）", self.paths.config_file.display());
            }
            Err(err) => self.fail_tab(Tab::Asr, err, "保存语音识别设置失败"),
        }
    }

    fn try_save_asr_tab(&self) -> anyhow::Result<AppConfig> {
        let asr = self.drafts.asr.clone();
        config_edit::update(&self.paths.config_file, |config| {
            asr.apply_to(config);
            Ok(())
        })
    }

    /// 放弃语音识别页的全部改动，恢复为配置文件里的值。
    pub fn reset_asr_tab(&mut self) {
        self.drafts.asr = AsrDraft::from_config(&self.config);
        self.errors.remove(&Tab::Asr);
    }

    // ---- 自己页 ----

    /// 「自己」页是否有未保存改动（麦克风切换即写回，不算脏）。
    pub fn self_tab_dirty(&self) -> bool {
        self.drafts.local.is_dirty(&self.config)
    }

    /// 保存「自己」页的其余改动（语言 / 翻译 / 合成，`[self]` 一次原子写回）。
    ///
    /// 麦克风已在切换时单独写回（D43），这里不改它。成功后重载配置；
    /// 运行中会按 D20 自动重启实时管线。
    pub fn save_self_tab(&mut self) {
        if !self.self_tab_dirty() {
            return;
        }
        match self.try_save_self_tab() {
            Ok(config) => {
                self.errors.remove(&Tab::Local);
                self.apply_config(config);
                info!("已保存「自己」设置（{}）", self.paths.config_file.display());
            }
            Err(err) => self.fail_tab(Tab::Local, err, "保存「自己」设置失败"),
        }
    }

    fn try_save_self_tab(&self) -> anyhow::Result<AppConfig> {
        let local = self.drafts.local.clone();
        config_edit::update(&self.paths.config_file, |config| {
            local.apply_to(config);
            Ok(())
        })
    }

    /// 放弃「自己」页的改动，恢复为配置文件里的值。
    pub fn reset_self_tab(&mut self) {
        self.drafts.local = LocalDraft::from_config(&self.config);
        self.errors.remove(&Tab::Local);
    }

    // ---- 他人页 ----

    /// 「他人」页是否有未保存改动（侦听开关切换即写回，不算脏）。
    pub fn others_tab_dirty(&self) -> bool {
        self.drafts.others.is_dirty(&self.config)
    }

    /// 保存「他人」页的其余改动（语言 / 翻译，`[others]` 一次原子写回）。
    ///
    /// 侦听开关已在切换时单独写回（D43），这里不改它。成功后重载配置；
    /// 运行中会按 D20 自动重启实时管线。
    pub fn save_others_tab(&mut self) {
        if !self.others_tab_dirty() {
            return;
        }
        match self.try_save_others_tab() {
            Ok(config) => {
                self.errors.remove(&Tab::Others);
                self.apply_config(config);
                info!("已保存「他人」设置（{}）", self.paths.config_file.display());
            }
            Err(err) => self.fail_tab(Tab::Others, err, "保存「他人」设置失败"),
        }
    }

    fn try_save_others_tab(&self) -> anyhow::Result<AppConfig> {
        let others = self.drafts.others.clone();
        config_edit::update(&self.paths.config_file, |config| {
            others.apply_to(config);
            Ok(())
        })
    }

    /// 放弃「他人」页的改动，恢复为配置文件里的值。
    pub fn reset_others_tab(&mut self) {
        self.drafts.others = OthersDraft::from_config(&self.config);
        self.errors.remove(&Tab::Others);
    }

    /// 重新枚举音频设备（启动时与「刷新设备」按钮调用）。
    pub fn refresh_devices(&mut self) {
        let manager = AudioDeviceManager::new();
        match manager.input_device_names() {
            Ok(names) => self.input_devices = names,
            Err(err) => warn!("枚举输入设备失败：{err:#}"),
        }
        match manager.output_device_names() {
            Ok(names) => self.output_devices = names,
            Err(err) => warn!("枚举输出设备失败：{err:#}"),
        }
    }

    /// 设置页是否有未保存改动（API Key + 高级设置）。
    pub fn settings_tab_dirty(&self) -> bool {
        self.drafts.settings.is_dirty(&self.config)
    }

    /// 整页保存设置：API Key、接入域名、缓存文件、分块词数。
    ///
    /// API Key 留空表示清除该配置项；其它字段由序列化器省略默认值。
    pub fn save_settings_tab(&mut self) {
        let settings = self.drafts.settings.clone();
        let result = self.write_config(|config| {
            settings.apply_to(config);
            Ok(())
        });
        let saved = result.is_ok();
        self.finish_config_write(result, "已保存设置");
        if saved {
            self.api_key_notice = None;
        }
    }

    /// 放弃设置页的整页改动。
    pub fn reset_settings_tab(&mut self) {
        self.drafts.settings = SettingsDraft::from_config(&self.config);
        self.errors.remove(&Tab::Settings);
    }

    /// 翻译模型草稿是否与配置不同。
    pub fn translate_model_dirty(&self) -> bool {
        self.drafts.translate_model_dirty(&self.config)
    }

    /// 保存翻译模型；其余翻译字段仍由翻译页即时保存。
    pub fn save_translate_model(&mut self) {
        let model = self.drafts.translate_model.trim().to_string();
        if model.is_empty() {
            self.errors
                .insert(Tab::Translate, "翻译模型不能为空".to_string());
            return;
        }
        let result = self.write_config(|config| {
            config.translate.model = model;
            Ok(())
        });
        self.finish_config_write(result, "已保存翻译模型");
    }

    /// 音频页 VAD 草稿是否与配置不同。
    pub fn vad_settings_dirty(&self) -> bool {
        self.drafts.vad.is_dirty(&self.config)
    }

    /// 保存音频页 VAD 参数；改后按 D20 热重启实时管线。
    pub fn save_vad_settings(&mut self) {
        let vad = self.drafts.vad.clone();
        let result = self.write_config(|config| {
            vad.apply_to(config);
            Ok(())
        });
        self.finish_config_write(result, "已保存 VAD 设置");
    }

    /// 放弃音频页 VAD 草稿。
    pub fn reset_vad_settings(&mut self) {
        self.drafts.vad = VadDraft::from_config(&self.config);
        self.errors.remove(&Tab::Audio);
    }

    /// 更新 `[audio]` 设备（`None` = 系统默认）；设备名是完整名称（D12）。
    pub fn update_audio_devices(&mut self, input: Option<String>, output: Option<String>) {
        let result = self.write_config(|config| {
            config.audio.input_device = normalize_device(input);
            config.audio.output_device = normalize_device(output);
            Ok(())
        });
        self.finish_config_write(result, "已保存音频设备");
    }

    /// 更新翻译页负责的共用字段：`[translate]` 的粒度与是否显示原文（D3/D27/D38）。
    ///
    /// 每条线路的翻译开关与目标语言在「自己」/「他人」页；
    /// `show_source` 不进配置指纹，改后立即生效、不重启会话。
    pub fn update_translate_settings(
        &mut self,
        granularity: TranslateGranularity,
        show_source: bool,
    ) {
        let result = self.write_config(|config| {
            config.translate.granularity = granularity;
            config.translate.show_source = show_source;
            Ok(())
        });
        self.finish_config_write(result, "已保存翻译设置");
    }

    /// 更新 VRChat 页负责的 `[vrchat.osc]` 字段（D3/D33/D42）。
    ///
    /// `[vrchat.osc]` 不进配置指纹，改设置立即生效、不重启实时管线。
    pub fn update_vrchat_settings(
        &mut self,
        enabled: bool,
        port: u16,
        display: VrchatDisplay,
        show_history: bool,
        retention_secs: u32,
    ) {
        let result = self.write_config(|config| {
            config.vrchat.osc.enabled = enabled;
            config.vrchat.osc.port = port;
            config.vrchat.osc.display = display;
            config.vrchat.osc.show_history = show_history;
            config.vrchat.osc.retention_secs = retention_secs;
            Ok(())
        });
        self.finish_config_write(result, "已保存 VRChat 设置");
    }

    /// 保存「VRChat」页的目标进程（`[vrchat] process`）。
    ///
    /// 它是对方线路的采集目标、进配置指纹，运行中保存会触发 D20 热重启；
    /// 因此用「文本框 + 应用」而不是逐键写回。
    pub fn save_vrchat_process(&mut self) {
        let process = self.drafts.vrchat_process.trim().to_string();
        if process.is_empty() {
            self.errors.insert(
                Tab::Vrchat,
                "目标进程不能为空（例如 VRChat.exe）".to_string(),
            );
            return;
        }
        if process == self.config.vrchat.process {
            return;
        }
        let result = self.write_config(|config| {
            config.vrchat.process = process.clone();
            Ok(())
        });
        self.finish_config_write(result, "已保存 VRChat 监听进程");
    }

    /// 立即发送一条 VRChat 测试消息（不走 1 秒文本节流）。
    pub fn send_test_message(&self) {
        if self.config.vrchat.osc.enabled {
            self.chatbox.send_test();
        }
    }

    /// 手动清空 VRChat 聊天框（含历史句，D44）；OSC 关闭时无操作。
    pub fn clear_chatbox(&self) {
        if self.config.vrchat.osc.enabled {
            self.chatbox.force_clear();
        }
    }

    // ---- 快捷开关（D43）----
    //
    // 顶栏图标与「自己 / 他人」页的 checkbox 共用这一条路径：切换即写回配置，
    // 同时通过 `PipelineSessionHandle` 通知会话按线路立即启停；成功后同步运行
    // 配置指纹，避免误触发 D20 全量热重启。

    /// 麦克风开关（顶栏图标 / 「自己」页 checkbox）：写回 `[self] mic` 并立即启停麦克风线路。
    pub fn set_local_mic(&mut self, enabled: bool) {
        if self.config.local.mic == enabled {
            return;
        }
        match self.write_config(|config| {
            config.local.mic = enabled;
            Ok(())
        }) {
            Ok(config) => {
                // 快捷开关不算 D20 热重启（D43）：直接换上新配置，再刷运行指纹
                self.config = config;
                if let Some(session) = &self.session {
                    session.set_mic_enabled(enabled);
                }
                self.refresh_running_signature();
                if !enabled {
                    self.vad_active = false;
                    self.timeline.drop_open(Speaker::Local);
                }
                self.config_write_error = None;
                info!(
                    "麦克风已{}（{}）",
                    if enabled { "开启" } else { "关闭" },
                    self.paths.config_file.display()
                );
            }
            Err(err) => {
                let message = format!("{err:#}");
                self.config_write_error = Some(message.clone());
                warn!("切换麦克风失败：{message}");
            }
        }
    }

    /// 他人侦听开关（顶栏图标 / 「他人」页 checkbox）：写回 `[others] enabled` 并立即启停他人线路。
    pub fn set_others_enabled(&mut self, enabled: bool) {
        if self.config.others.enabled == enabled {
            return;
        }
        match self.write_config(|config| {
            config.others.enabled = enabled;
            Ok(())
        }) {
            Ok(config) => {
                // 快捷开关不算 D20 热重启（D43）：直接换上新配置，再刷运行指纹
                self.config = config;
                if let Some(session) = &self.session {
                    session.set_others_enabled(enabled);
                }
                self.refresh_running_signature();
                if !enabled {
                    self.vad_active_others = false;
                    self.others_status = None;
                    self.others_retry_deadline = None;
                    self.timeline.drop_open(Speaker::Others);
                }
                self.config_write_error = None;
                info!(
                    "他人侦听已{}（{}）",
                    if enabled { "开启" } else { "关闭" },
                    self.paths.config_file.display()
                );
            }
            Err(err) => {
                let message = format!("{err:#}");
                self.config_write_error = Some(message.clone());
                warn!("切换他人侦听失败：{message}");
            }
        }
    }

    /// 快捷开关已由监督者接管，更新运行指纹避免 D20 误重启。
    fn refresh_running_signature(&mut self) {
        if self.session.is_some() {
            self.running_config_signature = Some(config_signature(&self.config));
        }
    }

    /// 麦克风线路启动失败：监督者已把运行时开关置关，这里把配置与页签一起回退。
    fn revert_local_mic_after_error(&mut self, detail: &str) {
        self.config.local.mic = false;
        if let Some(session) = &self.session {
            session.set_mic_enabled(false);
        }
        self.vad_active = false;
        self.timeline.drop_open(Speaker::Local);
        self.refresh_running_signature();
        match self.write_config(|config| {
            config.local.mic = false;
            Ok(())
        }) {
            Ok(config) => self.config = config,
            Err(err) => {
                let message = format!("{err:#}");
                self.config_write_error = Some(message.clone());
                warn!("麦克风启动失败后写回关闭状态失败：{message}");
            }
        }
        self.pipeline_warning = Some(format!("麦克风线路启动失败，已自动关闭：{detail}"));
        warn!("麦克风线路启动失败，已自动关闭：{detail}");
    }

    /// 读原文 → 解析为类型化配置 → 修改 → 最小化序列化并写回；返回写好的配置。
    ///
    /// 调用方把返回值交给 [`Self::apply_config`]（或直接赋给 `config`），不要重新读盘。
    fn write_config(
        &mut self,
        edit: impl FnOnce(&mut AppConfig) -> anyhow::Result<()>,
    ) -> anyhow::Result<AppConfig> {
        config_edit::update(&self.paths.config_file, edit)
    }

    fn finish_config_write(&mut self, result: anyhow::Result<AppConfig>, success_message: &str) {
        match result {
            Ok(config) => {
                self.errors.remove(&self.tab);
                self.apply_config(config);
                info!("{success_message}（{}）", self.paths.config_file.display());
            }
            Err(err) => self.fail_tab(self.tab, err, "写回配置失败"),
        }
    }

    /// 记一条页签级错误（页内红字）并写日志；`tab` 由调用方指定（不一定是当前页签）。
    fn fail_tab(&mut self, tab: Tab, err: anyhow::Error, context: &str) {
        let message = format!("{err:#}");
        self.errors.insert(tab, message.clone());
        warn!("{context}：{message}");
    }

    /// 某个页签的保存/校验错误（页内红字）；其它页签的错误不串页展示。
    pub fn error_for(&self, tab: Tab) -> Option<&str> {
        self.errors.get(&tab).map(String::as_str)
    }

    // ---- 自定义音色编辑 ----

    /// 打开程序目录（Windows 资源管理器）。
    pub fn open_exe_dir(&self) {
        if let Err(err) = std::process::Command::new("explorer")
            .arg(&self.paths.exe_dir)
            .spawn()
        {
            warn!("打开程序目录失败：{err}");
        }
    }

    // ---- 实时管线（PipelineSession）----

    /// 翻译模式字幕是否同时显示原文（只影响渲染，不参与配置指纹，D26）。
    pub fn translate_show_source(&self) -> bool {
        self.config.translate.show_source
    }

    /// 把配置里的两条线路翻译开关同步到时间线（空闲时即时生效，启动时供会话解释事件）。
    fn apply_translation_modes(&mut self) {
        self.timeline
            .set_translation(Speaker::Local, self.config.local.translate);
        self.timeline
            .set_translation(Speaker::Others, self.config.others.translate);
    }

    /// 「开始」：先建事件通道，再把会话丢到 tokio runtime 里跑。
    pub fn start_session(&mut self, ctx: egui::Context) {
        if self.session.is_some() {
            return;
        }
        // 未配置 Key 不算配置无效：配置本身是合法的，只是还不能启动实时会话。
        if self.config.api.key.as_deref().is_none_or(str::is_empty) {
            self.tab = Tab::Settings;
            self.api_key_notice = Some("尚未配置 API Key；填写后即可开始实时会话。".to_string());
            info!("未配置 API Key，已跳转到设置页");
            return;
        }
        self.api_key_notice = None;
        let config = self.config.clone();

        let (tx, rx) = mpsc::unbounded_channel();
        // 打字输入命令通道：GUI 提交、会话调度循环消费（无界，界面不会阻塞）。
        let (typed_tx, typed_rx) = mpsc::unbounded_channel::<TypedInput>();
        let handle = PipelineSessionHandle::new(typed_tx, config.local.mic, config.others.enabled);
        self.restart_pending = false;
        self.running_config_signature = Some(config_signature(&config));
        self.session = Some(handle.clone());
        self.events_rx = Some(rx);
        self.state = SessionState::Starting;
        self.pipeline_error = None;
        self.pipeline_warning = None;
        self.typed_input_error = None;
        self.vad_active = false;
        // 本次会话两条线路各自的翻译模式决定时间线如何解释 Asr/Translation 事件（D26/D39）。
        self.apply_translation_modes();
        // 只清当前未完成的消息；历史保留（用户要求停止管线后不清空历史）
        self.timeline.reset_open();

        let runtime = self.runtime.handle().clone();
        let events_for_run = tx.clone();
        let chatbox_for_run = self.chatbox.event_sender();
        let chatbox_for_error = chatbox_for_run.clone();
        let repaint = ctx.clone();
        runtime.spawn(async move {
            if let Err(err) = crate::pipeline::session::run(
                config,
                events_for_run,
                Some(chatbox_for_run),
                repaint.clone(),
                handle,
                typed_rx,
            )
            .await
            {
                let detail = format!("{err:#}");
                // best-effort: 界面可能已退出，事件丢弃即可
                let _ = chatbox_for_error.send(PipelineEvent::StateChanged(SessionState::Failed));
                let _ = tx.send(PipelineEvent::Error {
                    context: "实时管线启动/运行失败".to_string(),
                    detail,
                });
                let _ = tx.send(PipelineEvent::StateChanged(SessionState::Failed));
                repaint.request_repaint();
            }
        });
        info!("正在启动实时管线…");
    }

    /// 「停止」：请求后台收尾；回到 Idle 要等 `StateChanged(Idle)` 事件。
    pub fn stop_session(&mut self) {
        if let Some(session) = &self.session {
            session.stop();
            self.state = SessionState::Stopping;
            // 停止按钮立即清空「当前句」（历史保留），不用等下次开始
            self.timeline.reset_open();
            info!("正在停止实时管线…");
        }
    }

    /// 「朗读」/ Enter：把实时页输入框里的一句打字输入交给会话排队朗读。
    ///
    /// 校验顺序：空文本 → 长度 → 会话是否运行中 → 命令通道是否仍存活。
    /// 提交成功后清空输入框并保持焦点；失败时把原因留在输入框下方（不弹全局横幅）。
    pub fn submit_typed_input(&mut self) {
        let text = self.typed_input.trim().to_string();
        if text.is_empty() {
            self.typed_input_error = None;
            return;
        }
        let chars = text.chars().count();
        if chars > MAX_TYPED_INPUT_CHARS {
            self.typed_input_error = Some(format!(
                "最多 {MAX_TYPED_INPUT_CHARS} 个字符（当前 {chars} 个）"
            ));
            return;
        }
        if !matches!(self.state, SessionState::Running) {
            self.typed_input_error = Some("先点工具栏「开始」启动实时会话，再朗读".to_string());
            return;
        }
        let Some(session) = &self.session else {
            self.typed_input_error = Some("实时会话已结束，请重新开始".to_string());
            return;
        };
        if session.speak_typed(text) {
            self.typed_input.clear();
            self.typed_input_error = None;
        } else {
            self.typed_input_error = Some("实时会话已结束，请重新开始".to_string());
        }
    }

    /// 运行中配置发生变化时：停掉旧会话，等 Idle 事件后用新配置自动重启。
    fn maybe_restart_session(&mut self) {
        if !matches!(self.state, SessionState::Starting | SessionState::Running) {
            return;
        }
        let signature = config_signature(&self.config);
        if self.running_config_signature.as_deref() == Some(signature.as_str()) {
            return;
        }
        if self.session.is_none() {
            return;
        }
        info!("配置已变更，正在用新配置重启实时管线…");
        self.restart_pending = true;
        self.stop_session();
    }

    /// 更新顶栏两条电平条的平滑值，并在运行期间维持约 30fps 重绘。
    fn pump_meters(&mut self, ctx: &egui::Context) {
        let (mic_target, others_target) = match &self.session {
            Some(session) => (session.mic_level(), session.others_level()),
            None => (0.0, 0.0),
        };
        let dt = ctx.input(|input| input.stable_dt).clamp(0.0, 0.1);
        self.mic_level = self.mic_meter.update(mic_target, dt);
        self.others_level = self.others_meter.update(others_target, dt);
        if self.session.is_some() && (self.config.local.mic || self.config.others.enabled) {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }

    /// 排空后台事件（每帧一次）。
    fn pump_events(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(rx) = self.events_rx.as_mut() {
            loop {
                match rx.try_recv() {
                    Ok(event) => events.push(event),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.events_rx = None;
        }
        for event in events {
            self.apply_event(event);
        }
    }

    fn apply_event(&mut self, event: PipelineEvent) {
        match event {
            PipelineEvent::StateChanged(state) => {
                self.state = state;
                if matches!(state, SessionState::Idle | SessionState::Failed) {
                    self.session = None;
                    self.running_config_signature = None;
                    self.vad_active = false;
                    self.vad_active_others = false;
                    self.mic_level = 0.0;
                    self.others_level = 0.0;
                    self.mic_meter.reset();
                    self.others_meter.reset();
                    self.others_status = None;
                    self.others_retry_deadline = None;
                    self.typed_input_error = None;
                    // 会话结束：清掉可能残留的「当前句」（历史保留）
                    self.timeline.reset_open();
                    // 配置变更触发的停止：旧会话已退出，用新配置自动重启
                    if self.restart_pending && !self.closing {
                        self.restart_pending = false;
                        if let Some(ctx) = self.egui_ctx.clone() {
                            info!("旧会话已退出，使用新配置重启实时管线");
                            self.start_session(ctx);
                        }
                    }
                }
            }
            PipelineEvent::Devices { input, output } => {
                info!("实时管线设备已打开：输入 {input} / 输出 {output}");
            }
            PipelineEvent::VoiceInfo { display } => info!("合成音色：{display}"),
            PipelineEvent::Asr {
                speaker,
                text,
                is_sentence_end,
                sent_bytes,
                language,
                sentence_id,
            } => {
                // 翻译模式下这里只刷新源文与「已送翻译」前缀，句终由 Translation 处理（D26）。
                self.timeline.apply_asr(
                    speaker,
                    &text,
                    is_sentence_end,
                    sent_bytes,
                    language,
                    sentence_id,
                    Instant::now(),
                );
            }
            PipelineEvent::TypedSentence { text, language } => {
                self.timeline.push_typed(text, language, Instant::now());
            }
            PipelineEvent::Translation {
                speaker,
                text,
                sent_bytes,
                is_sentence_end,
                language,
                sentence_id,
            } => {
                // 翻译模式：译文进主行，原文来自此前的 Asr 事件；句终时把双语消息定稿。
                self.timeline.apply_translation(
                    speaker,
                    &text,
                    sent_bytes,
                    is_sentence_end,
                    language,
                    sentence_id,
                    Instant::now(),
                );
            }
            PipelineEvent::VadActive { speaker, active } => match speaker {
                Speaker::Local => self.vad_active = active,
                Speaker::Others => self.vad_active_others = active,
            },
            PipelineEvent::OthersStatus(status) => {
                // 他人线路状态是实时状态（带倒计时），不是一次性告警：恢复后自动消失。
                self.others_retry_deadline = status.retry_in.map(|retry| Instant::now() + retry);
                if status.state == OthersLineState::Capturing {
                    debug!("他人语音：采集中（PID {:?}）", status.pid);
                } else {
                    debug!("他人语音：{:?} {}", status.state, status.detail.trim());
                }
                if status.state != OthersLineState::Capturing && self.config.others.enabled {
                    self.others_status = Some(status);
                } else {
                    self.others_status = None;
                }
            }
            PipelineEvent::LineError { speaker, detail } => match speaker {
                Speaker::Local => {
                    warn!("麦克风线路启动失败：{detail}");
                    if self.config.local.mic {
                        self.revert_local_mic_after_error(&detail);
                    } else {
                        self.pipeline_warning = Some(format!("麦克风线路启动失败：{detail}"));
                    }
                }
                Speaker::Others => {
                    self.pipeline_warning = Some(format!("他人线路启动失败：{detail}"));
                    warn!("他人线路启动失败：{detail}");
                }
            },
            PipelineEvent::Warning { context, detail } => {
                // 不影响会话继续的告警（如预编译热词降级，D31）：黄色横幅，可手动清除。
                self.pipeline_warning = Some(format!("{context}：{detail}"));
                warn!("{context}：{detail}");
            }
            PipelineEvent::Error { context, detail } => {
                self.pipeline_error = Some(format!("{context}：{detail}"));
                error!("{context}：{detail}");
            }
        }
    }

    /// 把日志模块累计的行搬到界面状态里（每帧一次，避免每帧复制全部历史）。
    fn pump_logs(&mut self) {
        for line in log_sink::take_lines() {
            if self.log.len() >= MAX_LOG_LINES {
                self.log.pop_front();
            }
            self.log.push_back(line);
        }
        if let Some(message) = log_sink::take_crash() {
            self.crash = Some(message);
        }
    }

    /// 关闭流程：先取消关闭并做收尾，下一帧再真正退出（D13）。
    fn request_close(&mut self, ctx: &egui::Context) -> bool {
        if self.closing {
            log_sink::flush();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return true;
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            self.closing = true;
            self.restart_pending = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.request_repaint();
            // D13：先请求停止实时管线（后台任务退出前会 drop 音频设备），再关窗。
            if let Some(session) = &self.session {
                session.stop();
            }
            self.chatbox.shutdown();
            info!("收到关闭请求，正在退出");
            return true;
        }
        false
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        gui::banners::show(self, ui);
        ui.horizontal(|ui| {
            for tab in Tab::ALL {
                if ui.selectable_label(self.tab == tab, tab.title()).clicked() {
                    self.tab = tab;
                }
            }
        });
        ui.separator();

        match self.tab {
            Tab::Realtime => gui::realtime_tab::show(self, ui),
            Tab::Audio => gui::audio_tab::show(self, ui),
            Tab::Local => gui::self_tab::show(self, ui),
            Tab::Others => gui::others_tab::show(self, ui),
            Tab::Asr => gui::asr_tab::show(self, ui),
            Tab::Tts => gui::tts_tab::show(self, ui),
            Tab::Vrchat => gui::vrchat_tab::show(self, ui),
            Tab::Translate => gui::translate_tab::show(self, ui),
            Tab::Settings => gui::settings_tab::show(self, ui),
        }
    }
}

impl eframe::App for BivoxApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump_logs();
        self.pump_events();
        self.pump_meters(ui.ctx());
        // 自动重启需要 Context 来 request_repaint（点击「开始」时也会传一份）
        self.egui_ctx = Some(ui.ctx().clone());

        let ctx = ui.ctx().clone();
        if self.request_close(&ctx) {
            return;
        }

        // 面板顺序：先上/下，最后 CentralPanel。
        egui::Panel::top("bivox_toolbar").show(ui, |ui| gui::toolbar::show(self, ui));
        // 日志面板高度可拖拽调整（分割线在日志面板上边缘）
        egui::Panel::bottom("bivox_log")
            .resizable(true)
            .default_size(180.0)
            .min_size(60.0)
            .show(ui, |ui| gui::log_panel::show(self, ui));
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));
    }
}

/// 配置指纹：用于判断「运行中配置是否变了」。
///
/// 不能直接 `format!("{config:?}")`：`custom_voices` 是 `HashMap`，Debug 输出顺序取决于
/// 每个实例的随机种子，两次解析出的相同配置也会被判定为"变了"（导致每次保存都重启）。
/// 这里手工拼一个**顺序稳定**的指纹，只包含会影响运行的内容。
fn config_signature(config: &AppConfig) -> String {
    use std::fmt::Write as _;

    let mut voices: Vec<_> = config.tts.custom_voices.iter().collect();
    voices.sort_by(|a, b| a.0.cmp(b.0));

    let mut signature = String::new();
    // best-effort: 写入 `String` 的 `write!` 不会失败（`fmt::Error` 只可能来自底层 writer）
    let _ = write!(
        signature,
        "key={:?}\n\
         host={}\n\
         asr={}\n\
         hotwords={:?}\n\
         self.mic={}\n\
         self.langs={:?}\n\
         self.translate={}\n\
         self.target={}\n\
         self.tts={}\n\
         others.enabled={}\n\
         others.langs={:?}\n\
         others.translate={}\n\
         others.target={}\n\
         vrchat.process={}\n\
         tts={}\n\
         voice={}\n\
         dialect={}\n\
         cache={:?}\n\
         input={:?}\n\
         output={:?}\n\
         vad={}/{}/{}/{}/{}\n\
         chunk={}\n\
         translate.model={}\n\
         translate.granularity={}\n",
        config.api.key,
        config.api.workspace_host,
        config.asr.model,
        config.asr.hotwords,
        config.local.mic,
        config.local.languages,
        config.local.translate,
        config.local.target,
        config.local.tts,
        config.others.enabled,
        config.others.languages,
        config.others.translate,
        config.others.target,
        config.vrchat.process,
        config.tts.model,
        config.tts.voice,
        config.tts.dialect,
        config.cache.file,
        config.audio.input_device,
        config.audio.output_device,
        config.vad.enabled,
        config.vad.threshold,
        config.vad.min_silence_ms,
        config.vad.tail_ms,
        config.vad.debug,
        config.chunking.words,
        config.translate.model,
        config.translate.granularity.label(),
    );
    // best-effort: 同上，写 `String` 不会失败
    for (name, voice) in voices {
        let _ = write!(signature, "voice:{name}:{:?}:", voice.name);
        for sample in &voice.samples {
            let _ = write!(
                signature,
                "{}={:?}@{:?};",
                sample.language, sample.audio, sample.rate
            );
        }
        signature.push('\n');
    }
    signature
}

/// 选择渲染后端（默认构建启用 `renderer-wgpu`）。
///
/// 只有 `--no-default-features --features renderer-glow` 构建才有 glow；
/// 那个组合是唯一支持 `EFRAME_SCREENSHOT_TO` 截图核对的（wgpu 后端上游未实现该调试特性）。
#[cfg(feature = "renderer-wgpu")]
fn preferred_renderer() -> eframe::Renderer {
    eframe::Renderer::Wgpu
}

/// 选择渲染后端（仅 glow 构建，用于截图核对/OpenGL 兜底）。
#[cfg(all(feature = "renderer-glow", not(feature = "renderer-wgpu")))]
fn preferred_renderer() -> eframe::Renderer {
    eframe::Renderer::Glow
}

/// 窗口图标（与 `assets/bivox.ico` 同设计：深色圆角底 + 绿色声波柱）。
///
/// 程序化生成，避免运行时依赖图片解码库；exe 文件图标由 `build.rs` + `assets/bivox.ico` 提供。
fn app_icon() -> egui::IconData {
    const SIZE: usize = 64;
    const RADIUS: f32 = 14.0;
    const BG: [u8; 4] = [31, 36, 48, 255];
    const GREEN: [u8; 4] = [60, 201, 111, 255];
    // 每根声波柱：(中心 x, 半高)
    const BARS: [(f32, f32); 5] = [
        (18.0, 14.0),
        (26.0, 7.0),
        (34.0, 19.0),
        (42.0, 10.0),
        (50.0, 5.0),
    ];

    let size = SIZE as f32;
    let mut rgba = vec![0u8; SIZE * SIZE * 4];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (xf, yf) = (x as f32 + 0.5, y as f32 + 0.5);
            // 圆角矩形判定：把点夹到内缩矩形上，看是否落在圆角半径内
            let dx = if xf < RADIUS {
                RADIUS - xf
            } else if xf > size - RADIUS {
                xf - (size - RADIUS)
            } else {
                0.0
            };
            let dy = if yf < RADIUS {
                RADIUS - yf
            } else if yf > size - RADIUS {
                yf - (size - RADIUS)
            } else {
                0.0
            };
            if dx * dx + dy * dy > RADIUS * RADIUS {
                continue;
            }
            let on_bar = BARS
                .iter()
                .any(|&(cx, half)| (xf - cx).abs() <= 2.0 && (yf - 32.0).abs() <= half);
            let color = if on_bar { GREEN } else { BG };
            let offset = (y * SIZE + x) * 4;
            rgba[offset..offset + 4].copy_from_slice(&color);
        }
    }
    egui::IconData {
        rgba,
        width: SIZE as u32,
        height: SIZE as u32,
    }
}

/// 启动阶段遇到无法继续的错误时，用原生弹窗告知用户后退出。
///
/// release 构建没有控制台，不能只写 stderr；这里在创建主界面之前就失败，
/// 避免 GUI 拿到一份不认识的配置后又把它覆盖回去。
fn show_fatal_dialog(title: &str, message: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// GUI 入口：完成启动准备后进入 eframe 事件循环。
pub fn run(paths: AppPaths) -> ExitCode {
    // 尽可能早地安装崩溃钩子：release 构建没有控制台，这是唯一的崩溃线索。
    log_sink::install_panic_hook(paths.crash_log.clone());

    let removed_logs =
        log_sink::cleanup_old_logs(&paths.log_dir, chrono::Local::now().date_naive());
    let startup_error = match log_sink::init(&paths.log_dir) {
        Ok(()) => None,
        Err(err) => Some(format!("初始化日志失败：{err}")),
    };
    if removed_logs > 0 {
        info!(
            "已清理 {removed_logs} 个过期日志文件（保留 {} 天）",
            log_sink::LOG_RETENTION_DAYS
        );
    }

    info!(
        "Bivox v{} 启动，程序目录 {}",
        env!("CARGO_PKG_VERSION"),
        paths.exe_dir.display()
    );
    if let Some(message) = startup_error.as_deref() {
        warn!("启动检查未通过：{message}");
    }

    let config_created = match paths.ensure_default_config() {
        Ok(created) => created,
        Err(err) => {
            let message = format!(
                "无法准备配置文件：\n{}\n\n{err}\n\n程序将退出。\n请确认该目录可写。",
                paths.config_file.display()
            );
            error!("{message}");
            show_fatal_dialog("Bivox 配置错误", &message);
            return ExitCode::FAILURE;
        }
    };

    // 配置加载/校验失败是致命错误：不创建主界面，避免后续写回覆盖未知配置。
    let config = match AppConfig::load(&paths.config_file) {
        Ok(config) => {
            info!("已加载配置：{}", paths.config_file.display());
            config
        }
        Err(err) => {
            let detail = format!("{err:#}");
            error!("配置加载失败：{detail}");
            let message = format!(
                "无法加载配置文件：\n{}\n\n{detail}\n\n程序将退出。\n请修正该文件；删除它后重启会生成默认配置。",
                paths.config_file.display()
            );
            show_fatal_dialog("Bivox 配置错误", &message);
            return ExitCode::FAILURE;
        }
    };
    if config_created {
        info!(
            "已生成默认配置（尚未配置 API Key）：{}",
            paths.config_file.display()
        );
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            error!("创建 tokio runtime 失败：{err}");
            return ExitCode::FAILURE;
        }
    };

    let native_options = eframe::NativeOptions {
        renderer: preferred_renderer(),
        viewport: egui::ViewportBuilder::default()
            .with_title(WINDOW_TITLE)
            .with_icon(app_icon())
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    let result = eframe::run_native(
        WINDOW_TITLE,
        native_options,
        Box::new(move |_cc| {
            Ok(Box::new(BivoxApp::new(
                paths,
                runtime,
                config,
                startup_error,
            )))
        }),
    );

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("GUI 启动失败：{err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_signature_is_stable_across_instances() {
        // `custom_voices` 是 HashMap，两次解析的同内容配置 Debug 顺序可能不同；
        // 指纹必须稳定，否则每次保存配置都会误判"配置已变更"而重启管线。
        let toml = r#"
[api]
key = "sk-test"

[tts.custom_voices."a"]
[[tts.custom_voices."a".samples]]
language = "zh"
audio = "a.mp3"

[tts.custom_voices."b"]
[[tts.custom_voices."b".samples]]
language = "ja"
audio = "b.mp3"

[tts.custom_voices."c"]
[[tts.custom_voices."c".samples]]
language = "en"
audio = "c.mp3"
"#;

        let first = crate::config::AppConfig::from_toml_str(toml).unwrap();
        let signature = config_signature(&first);
        for _ in 0..20 {
            let another = crate::config::AppConfig::from_toml_str(toml).unwrap();
            assert_eq!(config_signature(&another), signature);
        }
    }

    #[test]
    fn config_signature_detects_changes() {
        let toml = r#"
[api]
key = "sk-test"

[audio]
input_device = "mic-a"
"#;
        let base = crate::config::AppConfig::from_toml_str(toml).unwrap();
        let changed =
            crate::config::AppConfig::from_toml_str(&toml.replace("mic-a", "mic-b")).unwrap();

        assert_ne!(config_signature(&base), config_signature(&changed));
    }

    #[test]
    fn config_signature_detects_sample_rate_changes() {
        // 语速变化会影响运行期合成参数，必须计入指纹（D20 热重启依据）。
        let toml = r#"
[api]
key = "sk-test"

[tts.custom_voices.edgar]
[[tts.custom_voices.edgar.samples]]
language = "zh"
audio = "edgar-zh.wav"
rate = 1.0
"#;
        let base = crate::config::AppConfig::from_toml_str(toml).unwrap();
        let changed =
            crate::config::AppConfig::from_toml_str(&toml.replace("rate = 1.0", "rate = 1.5"))
                .unwrap();

        assert_ne!(config_signature(&base), config_signature(&changed));
    }

    #[test]
    fn config_signature_detects_translate_changes_but_ignores_show_source() {
        // 两条线路的翻译开关/目标语言、共用模型与粒度影响运行期行为，必须计入指纹
        // （D20 热重启依据）；show_source 只影响字幕渲染，不应触发重启。
        let toml = r#"
[api]
key = "sk-test"

[self]
translate = true
target = "en"

[others]
enabled = true
translate = true
target = "zh"

[translate]
show_source = true
"#;
        let base = crate::config::AppConfig::from_toml_str(toml).unwrap();

        let self_target_changed = crate::config::AppConfig::from_toml_str(
            &toml.replace("target = \"en\"", "target = \"ja\""),
        )
        .unwrap();
        assert_ne!(
            config_signature(&base),
            config_signature(&self_target_changed),
            "我方译文语言必须进指纹"
        );

        let others_lang_changed = crate::config::AppConfig::from_toml_str(
            &toml.replace("target = \"zh\"", "target = \"en\""),
        )
        .unwrap();
        assert_ne!(
            config_signature(&base),
            config_signature(&others_lang_changed),
            "对方译文语言必须进指纹"
        );

        let others_enabled_changed = crate::config::AppConfig::from_toml_str(
            &toml.replace("enabled = true", "enabled = false"),
        )
        .unwrap();
        assert_ne!(
            config_signature(&base),
            config_signature(&others_enabled_changed),
            "对方线路开关必须进指纹"
        );

        let show_changed = crate::config::AppConfig::from_toml_str(
            &toml.replace("show_source = true", "show_source = false"),
        )
        .unwrap();
        assert_eq!(config_signature(&base), config_signature(&show_changed));
    }

    #[test]
    fn config_signature_ignores_vrchat_osc_settings() {
        // OSC 开关/端口/显示模式由独立后台任务实时读取，不应触发 D20 重启。
        let toml = r#"
[api]
key = "sk-test"

[vrchat.osc]
enabled = false
port = 9000
display = "bilingual"
"#;
        let base = crate::config::AppConfig::from_toml_str(toml).unwrap();
        let changed = crate::config::AppConfig::from_toml_str(
            &toml
                .replace("enabled = false", "enabled = true")
                .replace("port = 9000", "port = 9001")
                .replace(r#"display = "bilingual""#, r#"display = "translation""#),
        )
        .unwrap();
        assert_eq!(config_signature(&base), config_signature(&changed));
    }

    #[test]
    fn config_signature_detects_dialect_changes() {
        // 方言影响运行期合成参数，必须计入指纹（D20 热重启依据）。
        let toml = r#"
[api]
key = "sk-test"

[tts]
dialect = "普通话"
"#;
        let base = crate::config::AppConfig::from_toml_str(toml).unwrap();
        let changed =
            crate::config::AppConfig::from_toml_str(&toml.replace("普通话", "东北话")).unwrap();

        assert_ne!(config_signature(&base), config_signature(&changed));
    }

    #[test]
    fn config_signature_detects_hotword_changes() {
        // 热词影响运行期 ASR 参数（vocabulary_id），必须计入指纹（D20/D31）。
        let toml = r#"
[api]
key = "sk-test"

[asr]
hotwords = ["原神"]
"#;
        let base = crate::config::AppConfig::from_toml_str(toml).unwrap();
        let changed = crate::config::AppConfig::from_toml_str(
            &toml.replace(r#"["原神"]"#, r#"["原神", "Kaoet"]"#),
        )
        .unwrap();

        assert_ne!(config_signature(&base), config_signature(&changed));
    }

    #[test]
    fn hotword_draft_is_normalized_like_config() {
        // 草稿按行拆分后走与配置相同的归一化：空行/重复项不算改动。
        let parse = |text: &str| {
            let lines: Vec<String> = text.lines().map(str::to_string).collect();
            crate::config::normalize_hotwords(&lines)
        };
        assert_eq!(
            parse(" 原神 \n\n原神\nKaoet\n"),
            vec!["原神".to_string(), "Kaoet".to_string()]
        );
        assert!(parse("  \n\n").is_empty());
    }
}
