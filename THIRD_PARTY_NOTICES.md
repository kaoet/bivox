# 第三方组件与许可声明

Bivox 的发布包（`bivox.exe`）内嵌/链接了以下第三方组件。按 MIT 许可要求，随附其版权与许可声明。

## Silero VAD（模型）

- 项目：<https://github.com/snakers4/silero-vad>
- 版本：v6.2.3（模型文件 `silero_vad_16k_op15.onnx`，取自 `master` 的 `src/silero_vad/data/`）
- 许可：MIT，Copyright (c) 2020-present Silero Team
- 说明：模型通过 `include_bytes!` 内嵌进 `bivox.exe`，用于本地语音活动检测。

```text
MIT License

Copyright (c) 2020-present Silero Team

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## ONNX Runtime

- 项目：<https://github.com/microsoft/onnxruntime>
- 版本：1.31（通过 `ort` crate 的 pyke 预编译包静态链接进 `bivox.exe`）
- 许可：MIT，Copyright (c) Microsoft Corporation
- 说明：ONNX Runtime 自身还包含第三方组件，完整声明见其仓库的
  [`ThirdPartyNotices.txt`](https://github.com/microsoft/onnxruntime/blob/main/ThirdPartyNotices.txt)。

```text
MIT License

Copyright (c) Microsoft Corporation

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## 其他 Rust 依赖

其余 Rust crate 的许可信息见仓库的 `Cargo.lock` 与各 crate 的仓库页面；它们以源码形式编译进
`bivox.exe`。
