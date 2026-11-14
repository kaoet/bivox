//! 语言相关工具：
//!
//! - 语言代码集合与 `instruction` 映射（百炼声音复刻 / 合成用）
//! - 中文方言集合与合成 `instruction` 映射（`[tts] dialect`）
//! - 对 ASR 文本做"脚本统计"，据此判断用户这一句在说什么语言
//!
//! 官方 ASR 接口只接受 `language_hints` 输入提示、**不返回识别到的语种**，
//! 因此这里在 ASR 文本上做后处理：用"主导脚本 + 非对称证据 + 粘性"判定语言，
//! 不等待整句、不回溯（判定在每句首个 TTS chunk 送出前完成）。

/// 百炼声音复刻/合成支持的语言代码（`qwen-audio-3.1-tts-flash` 的 16 种）。
pub const CLONING_LANGUAGES: &[&str] = &[
    "zh", "en", "fr", "de", "ja", "ko", "ru", "pt", "th", "id", "vi", "es", "it", "ms", "fil", "ar",
];

/// 实时 ASR 支持的语言代码（`qwen-audio-3.1-asr-flash-streaming` 的 `language_hints`）。
pub const ASR_LANGUAGES: &[&str] = &[
    "zh", "en", "ja", "ko", "vi", "th", "id", "ms", "tl", "hi", "ar", "fr", "de", "es", "pt", "ru",
    "it", "nl", "sv", "da", "fi", "no", "el", "pl", "cs", "hu", "ro", "bg", "hr", "sk",
];

/// 翻译模式的目标语言：与 TTS 官方支持的语言一致（翻出来必须念得出来）。
///
/// 限制在 16 种而不是 qwen-mt 的 92 种，是为了目标语言的译文一定能交给 TTS；
/// 等 TTS 覆盖扩展后再增加。
pub const TRANSLATION_LANGUAGES: &[&str] = CLONING_LANGUAGES;

/// 拉丁字母语系（文本无法可靠区分，统一取配置列表里第一个拉丁语言）。
const LATIN_LANGUAGES: &[&str] = &[
    "en", "fr", "de", "es", "pt", "it", "id", "ms", "vi", "tl", "fil", "nl", "sv", "da", "fi",
    "no", "pl", "cs", "hu", "ro", "hr", "sk",
];

/// 判断是否为合法声音复刻语言代码。
pub fn is_cloning_language(code: &str) -> bool {
    CLONING_LANGUAGES.contains(&code)
}

/// 判断是否为合法 ASR 语言代码。
pub fn is_asr_language(code: &str) -> bool {
    ASR_LANGUAGES.contains(&code)
}

/// ASR 代码 → 声音复刻代码的别名归一（例如菲律宾语 ASR 用 `tl`、复刻用 `fil`）。
pub fn canonical_language(code: &str) -> &str {
    match code {
        "tl" => "fil",
        other => other,
    }
}

/// 把 ASR 语言代码映射为合成接口能接受的 `language_hints` 值。
///
/// 16 种已知语言直接用；未知语言用 `other`（`qwen-audio-3.1-tts-flash` 支持）。
pub fn tts_language_hint(asr_code: &str) -> &str {
    let canonical = canonical_language(asr_code);
    if is_cloning_language(canonical) {
        canonical
    } else {
        "other"
    }
}

/// 语言代码的中文显示名（翻译目标语言下拉框、语音识别页语言列表用；未知代码原样返回）。
pub fn language_display_name(code: &str) -> &str {
    match canonical_language(code) {
        "zh" => "中文",
        "en" => "英语",
        "fr" => "法语",
        "de" => "德语",
        "ja" => "日语",
        "ko" => "韩语",
        "ru" => "俄语",
        "pt" => "葡萄牙语",
        "th" => "泰语",
        "id" => "印尼语",
        "vi" => "越南语",
        "es" => "西班牙语",
        "it" => "意大利语",
        "ms" => "马来语",
        "fil" => "菲律宾语",
        "ar" => "阿拉伯语",
        "hi" => "印地语",
        "nl" => "荷兰语",
        "sv" => "瑞典语",
        "da" => "丹麦语",
        "fi" => "芬兰语",
        "no" => "挪威语",
        "el" => "希腊语",
        "pl" => "波兰语",
        "cs" => "捷克语",
        "hu" => "匈牙利语",
        "ro" => "罗马尼亚语",
        "bg" => "保加利亚语",
        "hr" => "克罗地亚语",
        "sk" => "斯洛伐克语",
        other => other,
    }
}

/// 合成时按语言附带的固定指令（zh/en 不需要；其余语言官方建议声明语种）。
pub fn instruction_for(asr_code: &str) -> Option<&'static str> {
    let instruction = match canonical_language(asr_code) {
        "ja" => "请讲日语。",
        "ko" => "请讲韩语。",
        "fr" => "请讲法语。",
        "de" => "请讲德语。",
        "ru" => "请讲俄语。",
        "es" => "请讲西班牙语。",
        "pt" => "请讲葡萄牙语。",
        "it" => "请讲意大利语。",
        "th" => "请讲泰语。",
        "vi" => "请讲越南语。",
        "id" => "请讲印尼语。",
        "ms" => "请讲马来语。",
        "fil" => "请讲菲律宾语。",
        "ar" => "请讲阿拉伯语。",
        _ => return None,
    };
    Some(instruction)
}

/// 百炼 Qwen-Audio-TTS 支持的中文方言（官方文档列表，首个为默认「普通话」）。
///
/// 官方 API 没有方言参数，方言只能通过合成请求的 `instruction` 表达：
/// 复刻音色可用全表；系统音色是否支持取决于音色本身（见 README「中文方言」）。
/// 只认下表中的官方名称，不接受「粤语」这类别名。
pub const CHINESE_DIALECTS: &[&str] = &[
    "普通话",
    "广东话",
    "重庆话",
    "东北话",
    "甘肃话",
    "贵州话",
    "浙江话",
    "河北话",
    "河南话",
    "湖北话",
    "湖南话",
    "江西话",
    "宁波话",
    "宁夏话",
    "青岛话",
    "陕西话",
    "山西话",
    "山东话",
    "上海话",
    "四川话",
    "云南话",
];

/// 中文方言默认值：普通话 = 不附加方言指令。
pub const DEFAULT_CHINESE_DIALECT: &str = "普通话";

/// 判断是否为受支持的中文方言名（忽略首尾空白）。
pub fn is_chinese_dialect(name: &str) -> bool {
    CHINESE_DIALECTS.contains(&name.trim())
}

/// 中文方言对应的合成指令；普通话（或未知值）返回 `None`（不附加 `instruction`）。
pub fn chinese_dialect_instruction(dialect: &str) -> Option<&'static str> {
    let instruction = match dialect.trim() {
        "普通话" => return None,
        "广东话" => "请讲广东话。",
        "重庆话" => "请讲重庆话。",
        "东北话" => "请讲东北话。",
        "甘肃话" => "请讲甘肃话。",
        "贵州话" => "请讲贵州话。",
        "浙江话" => "请讲浙江话。",
        "河北话" => "请讲河北话。",
        "河南话" => "请讲河南话。",
        "湖北话" => "请讲湖北话。",
        "湖南话" => "请讲湖南话。",
        "江西话" => "请讲江西话。",
        "宁波话" => "请讲宁波话。",
        "宁夏话" => "请讲宁夏话。",
        "青岛话" => "请讲青岛话。",
        "陕西话" => "请讲陕西话。",
        "山西话" => "请讲山西话。",
        "山东话" => "请讲山东话。",
        "上海话" => "请讲上海话。",
        "四川话" => "请讲四川话。",
        "云南话" => "请讲云南话。",
        _ => return None,
    };
    Some(instruction)
}

/// 本句合成请求应携带的 `instruction`：
/// 中文用 `[tts] dialect`（普通话不附加），其余语种沿用内置语种指令。
pub fn tts_instruction(language: &str, dialect: &str) -> Option<&'static str> {
    if canonical_language(language) == "zh" {
        return chinese_dialect_instruction(dialect);
    }
    instruction_for(language)
}

/// 一段文本里各类文字的数量统计（只统计"文字"，忽略空白/数字/标点）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScriptStats {
    pub han: u32,
    pub kana: u32,
    pub hangul: u32,
    pub latin: u32,
    pub cyrillic: u32,
    pub arabic: u32,
    pub thai: u32,
    pub greek: u32,
    pub devanagari: u32,
    pub other: u32,
}

impl ScriptStats {
    /// 统计文本中的文字脚本。
    pub fn count(text: &str) -> Self {
        let mut stats = Self::default();
        for ch in text.chars() {
            match script_of(ch) {
                Script::Han => stats.han += 1,
                Script::Kana => stats.kana += 1,
                Script::Hangul => stats.hangul += 1,
                Script::Latin => stats.latin += 1,
                Script::Cyrillic => stats.cyrillic += 1,
                Script::Arabic => stats.arabic += 1,
                Script::Thai => stats.thai += 1,
                Script::Greek => stats.greek += 1,
                Script::Devanagari => stats.devanagari += 1,
                Script::Other => stats.other += 1,
                Script::Ignore => {}
            }
        }
        stats
    }

    /// 参与判定的文字总数。
    pub fn total(self) -> u32 {
        self.han
            + self.kana
            + self.hangul
            + self.latin
            + self.cyrillic
            + self.arabic
            + self.thai
            + self.greek
            + self.devanagari
            + self.other
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Han,
    Kana,
    Hangul,
    Latin,
    Cyrillic,
    Arabic,
    Thai,
    Greek,
    Devanagari,
    Other,
    Ignore,
}

fn in_range(ch: char, start: u32, end: u32) -> bool {
    let code = ch as u32;
    (start..=end).contains(&code)
}

fn script_of(ch: char) -> Script {
    if ch.is_whitespace() || ch.is_ascii_digit() || ch.is_ascii_punctuation() {
        return Script::Ignore;
    }
    // 假名（含半角片假名、片假名语音扩展）
    if in_range(ch, 0x3040, 0x30FF) || in_range(ch, 0x31F0, 0x31FF) || in_range(ch, 0xFF66, 0xFF9D)
    {
        return Script::Kana;
    }
    // 谚文
    if in_range(ch, 0x1100, 0x11FF)
        || in_range(ch, 0x3130, 0x318F)
        || in_range(ch, 0xA960, 0xA97F)
        || in_range(ch, 0xAC00, 0xD7AF)
        || in_range(ch, 0xD7B0, 0xD7FF)
    {
        return Script::Hangul;
    }
    // 汉字（含扩展 A / 兼容区 / 扩展 B 及以后）
    if in_range(ch, 0x4E00, 0x9FFF)
        || in_range(ch, 0x3400, 0x4DBF)
        || in_range(ch, 0xF900, 0xFAFF)
        || in_range(ch, 0x20000, 0x2FA1F)
        || ch == '\u{3005}'
    {
        return Script::Han;
    }
    // 拉丁字母（基本区 + 扩展）
    if ch.is_ascii_alphabetic() || in_range(ch, 0x00C0, 0x024F) || in_range(ch, 0x1E00, 0x1EFF) {
        return Script::Latin;
    }
    // 西里尔 / 阿拉伯 / 泰文 / 希腊 / 天城文：可按脚本映射到对应语言
    if in_range(ch, 0x0400, 0x052F) || in_range(ch, 0x2DE0, 0x2DFF) || in_range(ch, 0xA640, 0xA69F)
    {
        return Script::Cyrillic;
    }
    if in_range(ch, 0x0600, 0x06FF)
        || in_range(ch, 0x0750, 0x077F)
        || in_range(ch, 0x08A0, 0x08FF)
        || in_range(ch, 0xFB50, 0xFDFF)
        || in_range(ch, 0xFE70, 0xFEFF)
    {
        return Script::Arabic;
    }
    if in_range(ch, 0x0E00, 0x0E7F) {
        return Script::Thai;
    }
    if in_range(ch, 0x0370, 0x03FF) || in_range(ch, 0x1F00, 0x1FFF) {
        return Script::Greek;
    }
    if in_range(ch, 0x0900, 0x097F) || in_range(ch, 0xA8E0, 0xA8FF) {
        return Script::Devanagari;
    }
    if ch.is_alphanumeric() || ch.is_alphabetic() {
        return Script::Other;
    }
    Script::Ignore
}

/// 句子级语言追踪器。
///
/// 规则（用户已确认）：
/// - 假名出现 → ja；谚文出现 → ko（这两种字符不会被中文/英文使用，属于强证据）；
/// - 汉字句：当前是 ja 且**汉字数 ≥ 6 且无假名**时切到 zh，否则保持当前（粘性）；
/// - 拉丁字母必须"主导且无 CJK"才算证据（避免 "U盘" 这类误切）；
/// - 文本太短（<2 个文字）或证据不足时保持当前语言；
/// - 检测结果不在允许列表内 → 落到 `allowed[0]`。
#[derive(Debug, Clone)]
pub struct LanguageTracker {
    allowed: Vec<String>,
    current: String,
}

impl LanguageTracker {
    /// `allowed` 必须非空（配置层已校验），首个元素为默认语言。
    pub fn new(allowed: Vec<String>) -> Self {
        debug_assert!(!allowed.is_empty(), "allowed languages 不能为空");
        let current = allowed.first().cloned().unwrap_or_else(|| "zh".to_string());
        Self { allowed, current }
    }

    /// 当前判定的语言（ASR 代码）。
    pub fn current(&self) -> &str {
        &self.current
    }

    /// 用当前已累积的句子文本更新判定，返回本句应使用的语言。
    ///
    /// 同一句内多次调用只会更新内部状态；调用方应在句子首个 chunk 送出前调用一次并冻结结果。
    pub fn decide(&mut self, text: &str) -> &str {
        let stats = ScriptStats::count(text);
        if stats.total() < 2 {
            return &self.current;
        }
        if let Some(candidate) = self.candidate(stats) {
            let candidate = self.normalize(candidate);
            if candidate != self.current {
                self.current = candidate;
            }
        }
        &self.current
    }

    fn normalize(&self, candidate: String) -> String {
        if self.allowed.iter().any(|lang| lang == &candidate) {
            candidate
        } else {
            // 检测到的语言不在用户声明的语言列表内 → 落到列表首个（默认语言）
            self.allowed[0].clone()
        }
    }

    /// 拉丁字母主导时：取 allowed 中第一个拉丁语言；若列表里没有拉丁语言则返回 en
    /// （随后由 normalize 落到 allowed[0]）。
    fn first_latin_allowed_or_en(&self) -> String {
        self.allowed
            .iter()
            .find(|lang| LATIN_LANGUAGES.contains(&lang.as_str()))
            .cloned()
            .unwrap_or_else(|| "en".to_string())
    }

    /// 返回"检测到的语言"（不做 allowed 过滤），由 [`Self::normalize`] 统一处理回退。
    fn candidate(&self, stats: ScriptStats) -> Option<String> {
        // 1) 假名 / 谚文：强证据（其它语言不会使用这些字符）
        if stats.kana > 0 {
            return Some("ja".to_string());
        }
        if stats.hangul > 0 {
            return Some("ko".to_string());
        }

        // 2) 汉字（与拉丁字母比较谁主导）
        if stats.han > stats.latin && stats.han > 0 {
            // 日语纯汉字短句：保持粘性；长汉字句且无假名 → 更可能是中文
            if self.current == "ja" {
                if stats.kana == 0 && stats.han >= 6 {
                    return Some("zh".to_string());
                }
                return None;
            }
            return Some("zh".to_string());
        }

        // 3) 拉丁字母主导（"U盘"这种单个字母混在汉字里不算）
        if stats.latin >= 2 && stats.latin > stats.han {
            return Some(self.first_latin_allowed_or_en());
        }

        // 4) 其它文字：按 Unicode 脚本映射
        if stats.cyrillic > 0 {
            return Some("ru".to_string());
        }
        if stats.arabic > 0 {
            return Some("ar".to_string());
        }
        if stats.thai > 0 {
            return Some("th".to_string());
        }
        if stats.greek > 0 {
            return Some("el".to_string());
        }
        if stats.devanagari > 0 {
            return Some("hi".to_string());
        }
        if stats.other > 0 {
            return Some(self.allowed[0].clone());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker(languages: &[&str]) -> LanguageTracker {
        LanguageTracker::new(languages.iter().map(|lang| lang.to_string()).collect())
    }

    #[test]
    fn script_stats_counts_expected_scripts() {
        let stats = ScriptStats::count("こんにちは、世界。Hello 123 U盘");
        assert_eq!(stats.kana, 5);
        assert_eq!(stats.han, 3, "世界 + 盘");
        assert_eq!(stats.latin, 6, "Hello + U 共 6 个拉丁字母");
    }

    #[test]
    fn kana_is_strong_evidence_for_japanese() {
        let mut t = tracker(&["zh", "ja"]);
        assert_eq!(t.decide("あ、我忘记带水壶了。"), "ja");
        assert_eq!(t.decide("今日はいい天気ですね。"), "ja");
    }

    #[test]
    fn hangul_is_strong_evidence_for_korean() {
        let mut t = tracker(&["zh", "ko"]);
        assert_eq!(t.decide("안녕하세요, 오늘 날씨가 좋네요."), "ko");
    }

    #[test]
    fn pure_han_sentence_switches_back_from_ja_after_six_chars() {
        // 用户场景 1：先说了一句含假名的日语，然后连续说中文
        let mut t = tracker(&["zh", "ja"]);
        assert_eq!(t.decide("あ、我忘记带水壶了。"), "ja");
        // 纯汉字、7 字 ≥ 6 → 切回中文
        assert_eq!(t.decide("今天天气真不错。"), "zh");
        assert_eq!(t.decide("我们一起去散步吧。"), "zh");
    }

    #[test]
    fn short_han_sentence_keeps_current_language() {
        let mut t = tracker(&["zh", "ja"]);
        assert_eq!(t.decide("あ、我忘记带水壶了。"), "ja");
        // 短汉字句（< 6 字）保持粘性，避免把"东京"这类日语名词误判为中文
        assert_eq!(t.decide("东京"), "ja");
    }

    #[test]
    fn latin_inside_chinese_does_not_switch_to_english() {
        // 用户场景 2："U盘" 里的单个拉丁字母不是强证据
        let mut t = tracker(&["zh", "ja", "en"]);
        assert_eq!(t.decide("我找不到我的 U盘了。"), "zh");
        assert_eq!(t.decide("OK，我明白了。"), "zh");
    }

    #[test]
    fn latin_dominant_sentence_switches_to_first_latin_language() {
        let mut t = tracker(&["zh", "ja", "en"]);
        assert_eq!(t.decide("Hello, nice to meet you."), "en");
        assert_eq!(t.decide("今天天气真不错。"), "zh");
    }

    #[test]
    fn latin_name_inside_english_keeps_english() {
        let mut t = tracker(&["zh", "en"]);
        assert_eq!(t.decide("I saw 王伟 yesterday."), "en");
    }

    #[test]
    fn single_language_config_forces_that_language() {
        let mut t = tracker(&["ja"]);
        assert_eq!(t.decide("今天天气真不错。"), "ja");
        assert_eq!(t.decide("Hello world"), "ja");
        assert_eq!(t.current(), "ja");
    }

    #[test]
    fn detected_language_outside_allowed_falls_back_to_first() {
        let mut t = tracker(&["zh", "en"]);
        assert_eq!(t.decide("hello world"), "en");
        // 检测到日语但列表里没有 ja → 立刻回落到列表首个（zh）
        assert_eq!(t.decide("こんにちは、世界。"), "zh");
        assert_eq!(t.decide("hello again"), "en");

        let mut t = tracker(&["zh", "ja"]);
        assert_eq!(t.decide("こんにちは"), "ja");
        // 检测到韩语但不在列表 → zh
        assert_eq!(t.decide("안녕하세요"), "zh");

        let mut t = tracker(&["zh", "ja"]);
        assert_eq!(t.decide("あ、我忘记带水壶了。"), "ja");
        // 检测到英语但不在列表 → zh
        assert_eq!(t.decide("Hello world"), "zh");
    }

    #[test]
    fn other_scripts_map_to_matching_language_when_allowed() {
        let mut t = tracker(&["zh", "ru"]);
        assert_eq!(t.decide("Привет, как дела?"), "ru");

        let mut t = tracker(&["zh", "ar"]);
        assert_eq!(t.decide("مرحبا بك"), "ar");

        let mut t = tracker(&["zh", "th"]);
        assert_eq!(t.decide("สวัสดีครับ"), "th");

        let mut t = tracker(&["zh", "el"]);
        assert_eq!(t.decide("Γειά σου"), "el");

        let mut t = tracker(&["zh", "hi"]);
        assert_eq!(t.decide("नमस्ते"), "hi");
    }

    #[test]
    fn very_short_text_keeps_current() {
        let mut t = tracker(&["zh", "ja"]);
        assert_eq!(t.decide("あ、我忘记带水壶了。"), "ja");
        // 单个汉字不足以作为证据
        assert_eq!(t.decide("空"), "ja");
    }

    #[test]
    fn tts_hint_and_instruction_mapping() {
        assert_eq!(tts_language_hint("ja"), "ja");
        assert_eq!(tts_language_hint("tl"), "fil");
        assert_eq!(tts_language_hint("no"), "other");
        assert_eq!(instruction_for("ja"), Some("请讲日语。"));
        assert_eq!(instruction_for("en"), None);
        assert_eq!(instruction_for("zh"), None);
        assert_eq!(instruction_for("no"), None);
    }

    #[test]
    fn chinese_dialects_are_official_and_unique() {
        assert_eq!(
            CHINESE_DIALECTS.first().copied(),
            Some(DEFAULT_CHINESE_DIALECT)
        );
        assert_eq!(CHINESE_DIALECTS.len(), 21, "普通话 + 官方 20 种方言");
        let mut seen = std::collections::HashSet::new();
        for dialect in CHINESE_DIALECTS {
            assert!(seen.insert(*dialect), "方言重复: {dialect}");
            assert!(is_chinese_dialect(dialect));
        }
        assert!(is_chinese_dialect("  东北话  "), "忽略首尾空白");
        // 只认官方名称：官方写法是「广东话」，不接受「粤语」别名
        assert!(!is_chinese_dialect("粤语"));
        assert!(!is_chinese_dialect("东北"));
    }

    #[test]
    fn chinese_dialect_instruction_mapping() {
        assert_eq!(chinese_dialect_instruction("普通话"), None);
        assert_eq!(chinese_dialect_instruction("东北话"), Some("请讲东北话。"));
        assert_eq!(chinese_dialect_instruction("广东话"), Some("请讲广东话。"));
        assert_eq!(chinese_dialect_instruction("上海话"), Some("请讲上海话。"));
        assert_eq!(chinese_dialect_instruction("粤语"), None, "不支持别名");
        assert_eq!(chinese_dialect_instruction("xx"), None);
    }

    #[test]
    fn tts_instruction_uses_dialect_only_for_chinese() {
        assert_eq!(tts_instruction("zh", "东北话"), Some("请讲东北话。"));
        assert_eq!(tts_instruction("zh", "普通话"), None);
        assert_eq!(tts_instruction("tl", "东北话"), Some("请讲菲律宾语。"));
        assert_eq!(tts_instruction("ja", "东北话"), Some("请讲日语。"));
        assert_eq!(tts_instruction("en", "东北话"), None);
        assert_eq!(tts_instruction("no", "东北话"), None);
    }

    #[test]
    fn known_language_sets_overlap() {
        assert!(is_asr_language("zh") && is_cloning_language("zh"));
        assert!(!is_asr_language("fil"), "ASR 用 tl，不用 fil");
        assert!(!is_cloning_language("hi"), "复刻不支持印地语");
        assert_eq!(canonical_language("tl"), "fil");
    }

    #[test]
    fn translation_languages_match_tts_languages_with_display_names() {
        assert_eq!(TRANSLATION_LANGUAGES, CLONING_LANGUAGES);
        for code in TRANSLATION_LANGUAGES {
            let name = language_display_name(code);
            assert_ne!(name, *code, "目标语言 {code} 应有中文显示名");
        }
        assert_eq!(language_display_name("tl"), "菲律宾语");
        assert_eq!(language_display_name("xx"), "xx", "未知代码原样返回");
    }
}
