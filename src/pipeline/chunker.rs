pub struct TextChunker {
    emitted_text: String,
    chunk_word_threshold: usize,
    guard_tail_chars: usize,
    policy: ChunkPolicy,
}

/// 切分策略。
///
/// 默认策略保持现有“TTS 边说边合成”的行为；翻译模式按粒度选择更保守的策略
/// （见 docs/architecture.md 第 9 节 ChunkPolicy 与第 15 节翻译模式）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkPolicy {
    /// 是否允许按词数/字数兜底硬切。
    pub word_cut: bool,
    /// 是否允许遇到标点即切。
    pub punctuation_cut: bool,
    /// 标点切点距当前文本末尾不足该字符数时先不切（0 = 关闭）。
    pub punctuation_tail_guard: usize,
}

impl ChunkPolicy {
    /// 翻译模式 · clause：只按标点/句终切，且标点须离开 ASR 不稳定尾部。
    pub fn translation_clause() -> Self {
        Self {
            word_cut: false,
            punctuation_cut: true,
            punctuation_tail_guard: TextChunker::DEFAULT_GUARD_TAIL_CHARS,
        }
    }

    /// 翻译模式 · sentence：只在句终切。
    pub fn translation_sentence() -> Self {
        Self {
            word_cut: false,
            punctuation_cut: false,
            punctuation_tail_guard: 0,
        }
    }
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        Self {
            word_cut: true,
            punctuation_cut: true,
            punctuation_tail_guard: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    pub chunk_text: String,
    pub is_sentence_end: bool,
}

const SENTENCE_PUNCTUATION: &[char] = &[
    ',', '.', '?', '!', ';', ':', '，', '。', '？', '！', '；', '：', '…',
];

fn is_cjk_punctuation(c: char) -> bool {
    matches!(
        c,
        '，' | '。'
            | '？'
            | '！'
            | '；'
            | '：'
            | '、'
            | '…'
            | '—'
            | '～'
            | '“'
            | '”'
            | '‘'
            | '’'
            | '（'
            | '）'
            | '【'
            | '】'
            | '《'
            | '》'
    )
}

fn is_ignorable_char(c: char) -> bool {
    c.is_whitespace() || c.is_ascii_punctuation() || is_cjk_punctuation(c)
}

/// 在全量文本与已发射历史之间进行对齐，返回 full_text 中未消费内容的起始字节偏移
fn find_unconsumed_byte_offset(full_text: &str, emitted_text: &str) -> usize {
    if emitted_text.is_empty() {
        return 0;
    }

    // 快速路径：完全前缀匹配
    if full_text.starts_with(emitted_text) {
        return emitted_text.len();
    }

    let emit_chars: Vec<char> = emitted_text.chars().collect();
    let full_chars: Vec<(usize, char)> = full_text.char_indices().collect();

    let mut e_idx = 0;
    let mut f_idx = 0;
    let mut last_matched_f_byte = 0;

    while e_idx < emit_chars.len() && f_idx < full_chars.len() {
        let e_ch = emit_chars[e_idx];
        let (f_byte, f_ch) = full_chars[f_idx];

        if e_ch == f_ch {
            last_matched_f_byte = f_byte + f_ch.len_utf8();
            e_idx += 1;
            f_idx += 1;
        } else if is_ignorable_char(f_ch) && !is_ignorable_char(e_ch) {
            // full_text 中插入了标点符号（例如 ASR 补了顿号或逗号），跳过 full_text 标点
            f_idx += 1;
        } else if is_ignorable_char(e_ch) && !is_ignorable_char(f_ch) {
            // emitted 中有标点但 full_text 中被去除，跳过 emitted 标点
            e_idx += 1;
        } else if is_ignorable_char(e_ch) && is_ignorable_char(f_ch) {
            // 两者都是标点但不相等，均跳过
            last_matched_f_byte = f_byte + f_ch.len_utf8();
            e_idx += 1;
            f_idx += 1;
        } else {
            // 两个字符都是实质文字但不相等（ASR 进行了字词微调）
            // 尝试在接下来的滑动窗口 (最多 3 步) 内重新同步
            let mut synced = false;
            for offset in 1..=3 {
                if f_idx + offset < full_chars.len() && full_chars[f_idx + offset].1 == e_ch {
                    f_idx += offset;
                    synced = true;
                    break;
                }
                if e_idx + offset < emit_chars.len() && emit_chars[e_idx + offset] == f_ch {
                    e_idx += offset;
                    synced = true;
                    break;
                }
            }
            if !synced {
                // 若小窗口内无法重同步，假定为单字替换纠错，推进 1 步
                last_matched_f_byte = f_byte + f_ch.len_utf8();
                e_idx += 1;
                f_idx += 1;
            }
        }
    }

    // 若 emitted 末尾还有未消费的纯标点符号，且 full 此时也是标点，推进吃掉
    while f_idx < full_chars.len() && is_ignorable_char(full_chars[f_idx].1) {
        if e_idx < emit_chars.len() && is_ignorable_char(emit_chars[e_idx]) {
            let (f_byte, f_ch) = full_chars[f_idx];
            last_matched_f_byte = f_byte + f_ch.len_utf8();
            e_idx += 1;
            f_idx += 1;
        } else {
            break;
        }
    }

    last_matched_f_byte
}

/// 查找按空格分词的第 target_words 个词结束处的字节索引
fn find_word_boundary_cut(pending: &str, target_words: usize) -> Option<usize> {
    let mut in_word = false;
    let mut word_count = 0;

    for (idx, ch) in pending.char_indices() {
        if ch.is_whitespace() {
            if in_word {
                in_word = false;
                word_count += 1;
                if word_count == target_words {
                    return Some(idx);
                }
            }
        } else {
            in_word = true;
        }
    }
    None
}

fn contains_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        ('\u{4E00}'..='\u{9FFF}').contains(&c)
            || ('\u{3400}'..='\u{4DBF}').contains(&c)
            || ('\u{F900}'..='\u{FAFF}').contains(&c)
    })
}

impl TextChunker {
    /// 默认安全尾部余量字符数（防止 ASR 尾部浮动窗口被过早硬切）
    pub const DEFAULT_GUARD_TAIL_CHARS: usize = 3;

    pub fn new(chunk_word_threshold: usize) -> Self {
        Self {
            emitted_text: String::new(),
            chunk_word_threshold,
            guard_tail_chars: Self::DEFAULT_GUARD_TAIL_CHARS,
            policy: ChunkPolicy::default(),
        }
    }

    pub fn with_guard_tail(chunk_word_threshold: usize, guard_tail_chars: usize) -> Self {
        Self {
            emitted_text: String::new(),
            chunk_word_threshold,
            guard_tail_chars,
            policy: ChunkPolicy::default(),
        }
    }

    /// 使用指定切分策略（翻译模式用；默认策略见 [`ChunkPolicy::default`]）。
    pub fn with_policy(chunk_word_threshold: usize, policy: ChunkPolicy) -> Self {
        Self {
            emitted_text: String::new(),
            chunk_word_threshold,
            guard_tail_chars: Self::DEFAULT_GUARD_TAIL_CHARS,
            policy,
        }
    }

    /// 根据百炼流式返回的完整/增量文本，智能检测断句并吐出切片
    pub fn process_stream(&mut self, full_text: &str, is_sentence_end: bool) -> Vec<TextChunk> {
        if full_text.is_empty() {
            // 空文本 + 句终：作为外部重置信号（例如 ASR 重连后，上一句已无法续接，
            // 需要丢弃已消费上下文，避免新句子与旧状态错位）
            if is_sentence_end {
                self.reset();
            }
            return Vec::new();
        }

        let mut unconsumed_start = find_unconsumed_byte_offset(full_text, &self.emitted_text);

        // 容错兜底：如果历史记录很长，但当前文本过短且完全无法匹配，视为外部重置
        if self.emitted_text.chars().count() >= 6
            && full_text.len() < self.emitted_text.len()
            && unconsumed_start == 0
        {
            self.emitted_text.clear();
            unconsumed_start = 0;
        }

        if unconsumed_start >= full_text.len() {
            if is_sentence_end {
                self.reset();
            }
            return Vec::new();
        }

        let mut pending_slice = &full_text[unconsumed_start..];
        let mut chunks = Vec::new();

        if is_sentence_end {
            let trimmed = pending_slice.trim();
            if !trimmed.is_empty() {
                chunks.push(TextChunk {
                    chunk_text: trimmed.to_string(),
                    is_sentence_end: true,
                });
            }
            self.reset();
            return chunks;
        }

        // 循环切片：支持一次调用中产出多个已达到切分条件的完整小句
        while !pending_slice.is_empty() {
            let mut cut_point = None;

            // 1. 优先检查标点符号切分点（英文逗号、句号等以及中文标点）。
            //    翻译模式要求标点离开 ASR 不稳定尾部后再切：切点之后的字符数不足
            //    guard 时先跳过，等文本继续延展（句终分支不受此限制）。
            if self.policy.punctuation_cut {
                for (idx, ch) in pending_slice.char_indices() {
                    if SENTENCE_PUNCTUATION.contains(&ch) {
                        let cut = idx + ch.len_utf8();
                        let tail_chars = pending_slice[cut..].chars().count();
                        if tail_chars >= self.policy.punctuation_tail_guard {
                            cut_point = Some(cut);
                            break;
                        }
                    }
                }
            }

            // 2. 若未遇到标点，检查分词/字数阈值（应用 guard_tail 尾部保护，防止切到未稳定的浮动字）：
            if cut_point.is_none() && self.policy.word_cut {
                if contains_cjk(pending_slice) {
                    // 中文连续文字：超过设定的字符数阈值（默认 12 字 + guard_tail_chars）
                    let char_count = pending_slice.chars().count();
                    let cjk_threshold = self.chunk_word_threshold * 2;
                    if char_count >= cjk_threshold + self.guard_tail_chars {
                        if let Some((idx, ch)) = pending_slice.char_indices().nth(cjk_threshold - 1)
                        {
                            let mut cut_idx = idx + ch.len_utf8();
                            // 避免在中英混排时截断英文单词
                            if ch.is_ascii_alphanumeric() {
                                for (next_idx, next_ch) in pending_slice[cut_idx..].char_indices() {
                                    if !next_ch.is_ascii_alphanumeric() {
                                        cut_idx += next_idx;
                                        break;
                                    }
                                }
                            }
                            cut_point = Some(cut_idx);
                        }
                    }
                } else {
                    // 英文/西文空格分词：超过设定单词数（默认 6 词 + guard_words）
                    let words: Vec<&str> = pending_slice.split_whitespace().collect();
                    let guard_words = (self.guard_tail_chars / 2).max(1);

                    if words.len() >= self.chunk_word_threshold + guard_words {
                        if let Some(cut_idx) =
                            find_word_boundary_cut(pending_slice, self.chunk_word_threshold)
                        {
                            cut_point = Some(cut_idx);
                        }
                    }
                }
            }

            if let Some(cut_len) = cut_point {
                if cut_len == 0 {
                    break;
                }
                let chunk_content = pending_slice[..cut_len].trim();
                if !chunk_content.is_empty() {
                    chunks.push(TextChunk {
                        chunk_text: chunk_content.to_string(),
                        is_sentence_end: false,
                    });
                }
                self.emitted_text.push_str(&pending_slice[..cut_len]);
                pending_slice = &pending_slice[cut_len..];
            } else {
                break;
            }
        }

        chunks
    }

    pub fn reset(&mut self) {
        self.emitted_text.clear();
    }

    /// 已发射（已送入 TTS）的内容在 `full_text` 中对应的**字节前缀长度**。
    ///
    /// 返回值一定是 UTF-8 字符边界；用于界面上把"已送 TTS / 尚未送 TTS"两部分分色显示。
    pub fn emitted_byte_len(&self, full_text: &str) -> usize {
        find_unconsumed_byte_offset(full_text, &self.emitted_text).min(full_text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_asr_revision_and_punctuation_insertion_reproduction() {
        // 复现用户日志中的场景：默认安全尾部保护（guard_tail = 3 或 4）
        // 在尾部未收敛时进行保护，收到标点/句终时再输出，避免过早抢跑且不重复字
        let mut chunker = TextChunker::new(6); // cjk_threshold = 12, guard_tail = 3

        let mut all_chunks = Vec::new();

        // 1. "小时" -> 不满阈值
        all_chunks.extend(chunker.process_stream("小时", false));
        assert!(all_chunks.is_empty());

        // 2. "小时绿向率" -> 不满阈值
        all_chunks.extend(chunker.process_stream("小时绿向率", false));
        assert!(all_chunks.is_empty());

        // 3. "小松绿向来不擅长" -> ASR 纠错，不满阈值
        all_chunks.extend(chunker.process_stream("小松绿向来不擅长", false));
        assert!(all_chunks.is_empty());

        // 4. "小松绿向来不擅长与其他同学打交" (15字，>= 12+3)
        let c1 = chunker.process_stream("小松绿向来不擅长与其他同学打交", false);
        assert_eq!(c1.len(), 1);
        assert_eq!(c1[0].chunk_text, "小松绿向来不擅长与其他同");
        all_chunks.extend(c1);

        // 5. "小松绿向来不擅长与其他同学打交道，经常" -> 遇到逗号
        let c2 = chunker.process_stream("小松绿向来不擅长与其他同学打交道，经常", false);
        assert_eq!(c2.len(), 1);
        assert_eq!(c2[0].chunk_text, "学打交道，");
        all_chunks.extend(c2);

        // 6. ASR 尾部延展：未发射部分共 14 字（"经常独自跑到山上和小花小草园"）
        // 14 < 12 + 3 不满足切分条件，安全留在缓冲区，不抢跑切出不稳定的“小花小草”
        let c_pending = chunker.process_stream(
            "小松绿向来不擅长与其他同学打交道，经常独自跑到山上和小花小草园",
            false,
        );
        assert!(c_pending.is_empty());

        // 7. ASR 修正为包含顿号 "、"，尾部由 "园" 变成 "说话。"
        let c3 = chunker.process_stream(
            "小松绿向来不擅长与其他同学打交道，经常独自跑到山上和小花、小草说话。",
            true,
        );
        assert_eq!(c3.len(), 1);
        assert_eq!(c3[0].chunk_text, "经常独自跑到山上和小花、小草说话。");
        assert!(c3[0].is_sentence_end);
        all_chunks.extend(c3);

        // 验证整体拼接后没有任何重复字（特别是“草”）
        let full_result: String = all_chunks.into_iter().map(|c| c.chunk_text).collect();
        assert_eq!(
            full_result,
            "小松绿向来不擅长与其他同学打交道，经常独自跑到山上和小花、小草说话。"
        );
    }

    #[test]
    fn test_mid_sentence_cut_and_punctuation_insertion_no_duplicate() {
        // 测试当字数很多发生中间硬切后，ASR 依然在已发射区域插入顿号，验证绝不重复字
        let mut chunker = TextChunker::with_guard_tail(3, 1); // cjk_threshold = 6, guard_tail = 1

        // 1. 第一次切分："经常跑到山上和小花小草园" (12字 >= 6+1)
        // 切出前 6 字："经常跑到山上"
        let c1 = chunker.process_stream("经常跑到山上和小花小草园", false);
        assert_eq!(c1.len(), 1);
        assert_eq!(c1[0].chunk_text, "经常跑到山上");

        // 2. 第二次：ASR 在后半段插入了顿号，并将 "园" 修改为 "说话。"
        let c2 = chunker.process_stream("经常跑到山上和小花、小草说话。", true);
        assert_eq!(c2.len(), 1);
        assert_eq!(c2[0].chunk_text, "和小花、小草说话。");

        let full = format!("{}{}", c1[0].chunk_text, c2[0].chunk_text);
        assert_eq!(full, "经常跑到山上和小花、小草说话。");
    }

    #[test]
    fn test_find_unconsumed_byte_offset_with_punctuation_insertion() {
        let emitted = "经常跑到山上和小花小草";
        let full = "经常跑到山上和小花、小草说话。";
        let offset = find_unconsumed_byte_offset(full, emitted);
        let pending = &full[offset..];
        assert_eq!(pending, "说话。");
    }

    #[test]
    fn test_punctuation_split() {
        let mut chunker = TextChunker::new(6);
        let chunks = chunker.process_stream("你好，世界！", false);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chunk_text, "你好，");
        assert_eq!(chunks[1].chunk_text, "世界！");
    }

    #[test]
    fn test_asr_typo_correction_alignment() {
        // 测试 ASR 中途进行错别字微调纠错时的对齐恢复
        let mut chunker = TextChunker::new(3);
        let _ = chunker.process_stream("小小时绿向来不擅长与其他", false);
        // ASR 纠正了前面的 "小时" 为 "松"，并继续延展
        let c = chunker.process_stream("小松绿向来不擅长与其他同学打交道。", true);
        assert_eq!(c.len(), 1);
        // 不应该整句重新输出，而是承接未消费内容
        assert!(c[0].chunk_text.contains("打交道。"));
    }

    #[test]
    fn test_multiple_sentences_in_single_stream() {
        let mut chunker = TextChunker::new(6);
        let chunks = chunker.process_stream("第一句话。第二句话！第三句话？", false);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chunk_text, "第一句话。");
        assert_eq!(chunks[1].chunk_text, "第二句话！");
        assert_eq!(chunks[2].chunk_text, "第三句话？");
    }

    #[test]
    fn test_empty_sentence_end_resets_state() {
        // 模拟 ASR 重连：上一句只发了一半，之后收到空文本 + 句终作为重置信号，
        // 新会话的首句不应与旧状态错位/漏字。
        let mut chunker = TextChunker::new(6);
        assert!(chunker
            .process_stream("上一句只识别了一半", false)
            .is_empty());

        assert!(chunker.process_stream("", true).is_empty());

        let chunks = chunker.process_stream("新的一句话。", false);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_text, "新的一句话。");
    }

    #[test]
    fn test_english_words_split() {
        let mut chunker = TextChunker::new(3); // 3 词阈值
        let text = "one two three four five";
        let chunks = chunker.process_stream(text, false);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_text, "one two three");
    }

    #[test]
    fn test_translation_clause_waits_for_punctuation_to_leave_tail() {
        let mut chunker = TextChunker::with_policy(6, ChunkPolicy::translation_clause());
        // 逗号紧贴当前文本末尾（剩余 0 字 < guard 3）：先不切
        assert!(chunker.process_stream("你好，", false).is_empty());
        // 文本延展后逗号已离开尾部（剩余 3 字）：切出
        let chunks = chunker.process_stream("你好，世界啊", false);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_text, "你好，");
        assert!(!chunks[0].is_sentence_end);
        // 句终不受 guard 限制
        let chunks = chunker.process_stream("你好，世界啊。", true);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_text, "世界啊。");
        assert!(chunks[0].is_sentence_end);
    }

    #[test]
    fn test_translation_clause_never_hard_cuts_without_punctuation() {
        let mut chunker = TextChunker::with_policy(6, ChunkPolicy::translation_clause());
        let text = "字".repeat(40);
        // 没有标点：翻译模式不做兜底硬切
        assert!(chunker.process_stream(&text, false).is_empty());
        // 句终时整句一次切出
        let sentence = format!("{text}。");
        let chunks = chunker.process_stream(&sentence, true);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_text, sentence);
        assert!(chunks[0].is_sentence_end);
    }

    #[test]
    fn test_translation_sentence_only_cuts_at_sentence_end() {
        let mut chunker = TextChunker::with_policy(6, ChunkPolicy::translation_sentence());
        assert!(chunker.process_stream("你好，世界！", false).is_empty());
        let chunks = chunker.process_stream("你好，世界！第二句", true);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_text, "你好，世界！第二句");
        assert!(chunks[0].is_sentence_end);
    }
}
