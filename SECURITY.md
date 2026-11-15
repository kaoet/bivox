# 安全策略

## 报告漏洞

**不要**为安全问题开公开 issue。请用 GitHub 的 [Security Advisories](https://github.com/kaoet/bivox/security/advisories/new) 私密报告，或发邮件到 kaoet.ibe@outlook.com。

## 请特别注意的敏感数据

- `config.toml` 里是**明文百炼 API Key**。不要把整个程序目录发给别人，也不要把这个文件贴到 issue 或截图里。
- `logs\bivox-*.log` 可能包含接口错误、请求域名与识别/翻译文本；贴日志前先删掉 Key 与不想公开的对话内容。
- 自定义音色的样例音频、`cache.json` 里的复刻记录都属于本机状态，同样不要公开分享。

## 支持范围

只对最新发布版本提供修复；旧版本请先升级再确认问题是否仍存在。
