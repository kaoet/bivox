//! GUI 应用主体：启动准备、全局状态、面板布局、退出流程。
//!
//! 阶段说明：M0 搭好骨架（窗口 + 三个页签 + 日志面板 + 便携路径/崩溃处理）；
//! M1 接入配置写回（API Key / 音频设备）；实时管线（M2）、音色编辑（M3）后续接入。

use std::collections::VecDeque;
use std::path::Path;
use std::process::ExitCode;

use eframe::egui;
use log::{error, info, warn};

use crate::audio::device::AudioDeviceManager;
use crate::config::AppConfig;
use crate::config_edit;
use crate::gui;
use crate::log_sink::{self, LogLine, MAX_LOG_LINES};
use crate::paths::AppPaths;

/// 窗口标题。
pub const WINDOW_TITLE: &str = "kaols — 实时语音识别与合成";

/// 会话状态机（M2 起由 `PipelineSession` 驱动）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Starting,
    Running,
    Stopping,
    Failed,
}

impl SessionState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "空闲",
            Self::Starting => "启动中",
            Self::Running => "运行中",
            Self::Stopping => "停止中",
            Self::Failed => "已失败",
        }
    }

    pub fn color(self) -> egui::Color32 {
        match self {
            Self::Idle => egui::Color32::GRAY,
            Self::Starting | Self::Stopping => egui::Color32::from_rgb(0xd8, 0x9a, 0x1b),
            Self::Running => egui::Color32::from_rgb(0x3c, 0xc9, 0x6f),
            Self::Failed => egui::Color32::from_rgb(0xe0, 0x4b, 0x4b),
        }
    }
}

/// 页签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Realtime,
    Audio,
    Tts,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Realtime, Tab::Audio, Tab::Tts, Tab::Settings];

    pub fn title(self) -> &'static str {
        match self {
            Self::Realtime => "实时",
            Self::Audio => "音频",
            Self::Tts => "语音合成",
            Self::Settings => "设置",
        }
    }
}

/// 配置加载结果。
///
/// `Loaded` 装箱：`AppConfig` 结构较大，避免枚举尺寸被它主导（clippy::large_enum_variant）。
pub enum ConfigState {
    Loaded(Box<AppConfig>),
    Invalid { error: String },
}

/// 应用状态。
pub struct KaolsApp {
    pub paths: AppPaths,
    /// 后台运行时；M2 起用它 spawn ASR/TTS 任务（现在只保活，等待接入）。
    #[allow(dead_code)]
    pub runtime: tokio::runtime::Runtime,
    pub tab: Tab,
    pub state: SessionState,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub config: ConfigState,
    /// 语音合成页左栏当前选中的自定义音色（配置键名）；`None` = 未选中。
    pub selected_voice: Option<String>,
    /// 设置页的 API Key 草稿（点「保存」才写回 config.toml）。
    pub api_key_input: String,
    /// 设备下拉的可选项（启动时与「刷新设备」时重新枚举）。
    pub input_devices: Vec<String>,
    pub output_devices: Vec<String>,
    /// 最近一次写回配置失败的原因（成功时清空）。
    pub config_write_error: Option<String>,
    /// 本次启动是否刚刚生成了默认配置。
    pub config_created: bool,
    /// 启动检查发现的问题（写默认配置失败、初始化日志失败等）。
    pub startup_error: Option<String>,
    /// 运行期崩溃信息（panic 钩子写入）。
    pub crash: Option<String>,
    pub log: VecDeque<LogLine>,
    /// 已经收到关闭请求、正在收尾。
    pub closing: bool,
}

impl KaolsApp {
    fn new(
        paths: AppPaths,
        runtime: tokio::runtime::Runtime,
        startup_error: Option<String>,
        config_created: bool,
    ) -> Self {
        let config = load_config(&paths.config_file);
        // 配置缺失/无效时直接落在设置页，符合 D18（首次运行引导填 Key）。
        let tab = if config_created || matches!(config, ConfigState::Invalid { .. }) {
            Tab::Settings
        } else {
            Tab::Realtime
        };

        let mut app = Self {
            paths,
            runtime,
            tab,
            state: SessionState::Idle,
            input_device: None,
            output_device: None,
            config,
            selected_voice: None,
            api_key_input: String::new(),
            input_devices: Vec::new(),
            output_devices: Vec::new(),
            config_write_error: None,
            config_created,
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
    pub fn reload_config(&mut self) {
        self.config = load_config(&self.paths.config_file);
        self.sync_from_config();
        // 配置可能删掉了当前选中的音色；清空选择，语音合成页会重新判定。
        self.selected_voice = None;
        info!("已重新加载配置：{}", self.paths.config_file.display());
    }

    /// 把配置里的 Key / 设备同步到界面状态（加载、重载、写回成功后调用）。
    fn sync_from_config(&mut self) {
        match &self.config {
            ConfigState::Loaded(config) => {
                self.api_key_input = config.api.key.clone().unwrap_or_default();
                self.input_device = config.audio.input_device.clone();
                self.output_device = config.audio.output_device.clone();
            }
            ConfigState::Invalid { .. } => {
                self.api_key_input.clear();
                self.input_device = None;
                self.output_device = None;
            }
        }
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

    /// 保存 API Key 到 config.toml（写前校验、写前备份，见 D3）。
    pub fn save_api_key(&mut self) {
        let key = self.api_key_input.trim().to_string();
        if key.is_empty() {
            self.config_write_error = Some("API Key 不能为空".to_string());
            warn!("保存 API Key 失败：输入为空");
            return;
        }
        let result = self.write_config(|text| config_edit::set_api_key(text, Some(&key)));
        self.finish_config_write(result, "已保存 API Key");
    }

    /// 更新 `[audio]` 设备（`None` = 系统默认）；设备名是完整名称（D12）。
    pub fn update_audio_devices(&mut self, input: Option<String>, output: Option<String>) {
        let result = self.write_config(|text| {
            config_edit::set_audio_devices(text, input.as_deref(), output.as_deref())
        });
        self.finish_config_write(result, "已保存音频设备");
    }

    /// 读原文 → 定点修改 → 校验并写回。
    fn write_config(
        &mut self,
        edit: impl FnOnce(&str) -> anyhow::Result<String>,
    ) -> anyhow::Result<()> {
        let original = config_edit::read(&self.paths.config_file)?;
        let updated = edit(&original)?;
        config_edit::save(&self.paths.config_file, &original, &updated)
    }

    fn finish_config_write(&mut self, result: anyhow::Result<()>, success_message: &str) {
        match result {
            Ok(()) => {
                self.config_write_error = None;
                self.reload_config();
                self.config_created = false;
                info!("{success_message}（{}）", self.paths.config_file.display());
            }
            Err(err) => {
                let message = format!("{err:#}");
                self.config_write_error = Some(message.clone());
                warn!("写回配置失败：{message}");
            }
        }
    }

    /// 打开程序目录（Windows 资源管理器）。
    pub fn open_exe_dir(&self) {
        if let Err(err) = std::process::Command::new("explorer")
            .arg(&self.paths.exe_dir)
            .spawn()
        {
            warn!("打开程序目录失败：{err}");
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
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.request_repaint();
            // M2：在这里停止 PipelineSession（先停音频再关窗）
            info!("收到关闭请求，正在退出");
            return true;
        }
        false
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        if let Some(detail) = self.crash.clone() {
            gui::widgets::banner(
                ui,
                "程序发生崩溃",
                &format!("{detail}（详情见 crash.log）"),
                egui::Color32::from_rgb(0xe0, 0x4b, 0x4b),
            );
        }
        if let Some(detail) = self.startup_error.clone() {
            gui::widgets::banner(
                ui,
                "启动检查未通过",
                &detail,
                egui::Color32::from_rgb(0xd8, 0x9a, 0x1b),
            );
        }

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
            Tab::Tts => gui::tts_tab::show(self, ui),
            Tab::Settings => gui::settings_tab::show(self, ui),
        }
    }
}

impl eframe::App for KaolsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.pump_logs();

        let ctx = ui.ctx().clone();
        if self.request_close(&ctx) {
            return;
        }

        // 面板顺序：先上/下，最后 CentralPanel。
        egui::Panel::top("kaols_toolbar").show(ui, |ui| gui::toolbar::show(self, ui));
        // 日志面板高度可拖拽调整（分割线在日志面板上边缘）
        egui::Panel::bottom("kaols_log")
            .resizable(true)
            .default_size(180.0)
            .min_size(60.0)
            .show(ui, |ui| gui::log_panel::show(self, ui));
        egui::CentralPanel::default().show(ui, |ui| self.central(ui));
    }
}

/// 从指定路径加载配置，失败时返回可展示的错误文本。
pub fn load_config(path: &Path) -> ConfigState {
    match AppConfig::load(path) {
        Ok(config) => {
            info!("已加载配置：{}", path.display());
            ConfigState::Loaded(Box::new(config))
        }
        Err(err) => {
            let message = format!("{err:#}");
            warn!("配置加载失败：{message}");
            ConfigState::Invalid { error: message }
        }
    }
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

/// GUI 入口：完成启动准备后进入 eframe 事件循环。
pub fn run(paths: AppPaths) -> ExitCode {
    // 尽可能早地安装崩溃钩子：release 构建没有控制台，这是唯一的崩溃线索。
    log_sink::install_panic_hook(paths.crash_log.clone());

    let mut startup_error: Option<String> = None;
    let mut record_error = |message: String| {
        if startup_error.is_none() {
            startup_error = Some(message);
        }
    };

    let config_created = match paths.ensure_default_config() {
        Ok(created) => {
            if created {
                eprintln!("已生成默认配置：{}", paths.config_file.display());
            }
            created
        }
        Err(err) => {
            record_error(format!("写入默认配置失败：{err}"));
            false
        }
    };

    if let Err(err) = log_sink::init(&paths.log_dir) {
        record_error(format!("初始化日志失败：{err}"));
    }

    info!(
        "kaols v{} 启动，程序目录 {}",
        env!("CARGO_PKG_VERSION"),
        paths.exe_dir.display()
    );
    if config_created {
        info!(
            "已生成默认配置（[api] key 待填写）：{}",
            paths.config_file.display()
        );
    }
    if let Some(message) = startup_error.as_deref() {
        warn!("启动检查未通过：{message}");
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
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    let result = eframe::run_native(
        WINDOW_TITLE,
        native_options,
        Box::new(move |_cc| {
            Ok(Box::new(KaolsApp::new(
                paths,
                runtime,
                startup_error,
                config_created,
            )))
        }),
    );

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            error!("GUI 启动失败：{err}");
            eprintln!("GUI 启动失败：{err}");
            ExitCode::FAILURE
        }
    }
}
