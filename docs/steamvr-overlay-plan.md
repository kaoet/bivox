# SteamVR Overlay 实现跟踪（规划）

> 临时工作文档，**不是**规范事实源。功能落地后，将长期决策合并到
> [`docs/decisions.md`](decisions.md) 的下一个 `D<n>`（暂记 D46），当前实现合并到
> [`docs/architecture.md`](architecture.md) 与 [`README.md`](../README.md)，然后归档/删除本文件。
>
> 状态：**方案已定稿，等待用户明确指示后才开始改代码**。最后更新：2026-11-25。

---

## 1. 目标与非目标

### 目标（初版）

- 在「VRChat」页右侧新增「SteamVR」页签，提供 Overlay 开关与调试预览。
- 开关打开后，在 SteamVR 中显示一个 **固定在右手手背上方 10–20cm 的小面板**。
- 面板只显示实时页的我方/对方聊天气泡，**没有任何按钮或输入框**。
- 停止会话后仍保留历史气泡（与实时页一致）。
- 不依赖 GUI 重绘：即使 Bivox 主窗口最小化，Overlay 仍持续更新。
- 为后续按钮/输入预留完整链路；初版不启用输入。

### 非目标（初版不做）

- 不做头部锁定、世界固定、手柄尖端/手腕等其他放置方式。
- 不支持没有 controller pose 的纯手部追踪。
- 不启用系统激光/按钮/输入框/SteamVR 键盘。
- 不做 GPU（D3D11/D3D12）纹理共享，先用 `SetOverlayRaw`。
- 不自动启动 SteamVR。
- 不做 Dashboard 专用 Overlay。
- 不在 Overlay 里提供任何交互控件。

---

## 2. 已定决策

| 项目 | 决策 |
|---|---|
| 放置 | **只支持手柄相对固定**；默认右手手背，不使用 HeadLocked/WorldFixed |
| 理由 | 左手手背通常已有其他软件的 Overlay，所以默认右手 |
| 配置字段 | 用 `position`，默认 `right_back`；不再用 `hand` |
| 当前 position | 支持 `right_back`（默认）与 `left_back`；两者都是手背，手心等以后再加 |
| 激光手 | 默认右手面板 + **左手激光**；页面不写“激光请用左手”的提示 |
| 面板 | 悬浮在手背上方 10–20cm 的小面板，4:3 |
| 初版尺寸 | 宽 **0.28m**，纹理 **1024×768**，最多 **3 条气泡** |
| 字体 | Overlay 用 `bubble` 的 compact style（scale ≈ 1.3，隐藏外侧语言代码） |
| 输入 | 初版 `SetOverlayInputMethod(None)`；后续先试系统激光 |
| 渲染 | 自建 `egui::RawInput` 桥，`dream-soft-render` 只作为光栅化后端 |
| 刷新 | 只读 10 Hz；交互 20–30 Hz |
| 调试 | 桌面预览 + `overlay_debug --anchor front` + `overlay_debug --position right_back --tune` |
| 动作回传 | `OverlayAction` mpsc → `BivoxApp`，Overlay 线程不直接写配置 |
| 配置指纹 | `[overlay]` **不进**配置指纹，改开关/尺寸不触发 D20 重启 |
| 新页签 | 「SteamVR」，固定在 VRChat 右侧 |

已确认的交互/渲染细节：

- 按钮点击等动作走 `OverlayAction`，由 GUI 线程处理（D3/D5）。
- 渲染入口必须接受 `RawInput`，不能使用 `dream-soft-render::render_egui`（它内部自己构造
  `RawInput`，无法注入鼠标/键盘/文本事件）。
- `dream-soft-render` 的 framebuffer 是不透明的；继续用“黑/白两遍背景恢复 alpha”的方案。
- 系统激光先用 `VROverlayInputMethod_Mouse` + `VROverlayFlags_MakeOverlaysInteractiveIfVisible`；
  如果抢 VRChat 输入或选错手，再换自定义射线。

---

## 3. 配置草案

```toml
[overlay]
enabled = false
position = "right_back"
width_m = 0.28
max_bubbles = 3
```

| 字段 | 默认 | 范围/取值 | 说明 |
|---|---:|---|---|
| `enabled` | `false` | bool | 总开关；立即生效，不重启会话 |
| `position` | `"right_back"` | 当前：`right_back`（默认）/ `left_back` | 右手/左手手背；手心等以后追加 |
| `width_m` | `0.28` | 0.20–0.35 | 物理宽度；高度由 4:3 纹理决定 |
| `max_bubbles` | `3` | 1–5 | 最多显示几条最新气泡 |

约束：

- `[overlay]` 全部字段必须有 GUI 入口（D3）。
- `[overlay]` 不进配置指纹；`config_signature` 增加“忽略 overlay 设置”的测试。
- 不做旧配置兼容（D41）；schema 直接替换。
- 不在 GUI 写“激光请用左手”的提示（用户要求）。

---

## 4. 技术方案

### 4.1 OpenVR 加载与接口

- 动态加载 SteamVR 自带的 `openvr_api.dll`，不把 DLL 打进发布 zip（D2）。
- 通过 `%LOCALAPPDATA%\openvr\openvrpaths.vrpath` 的 `runtime[0]` 定位
  `<SteamVR>/bin/win64/openvr_api.dll`。这是 D4 的明确例外，只在 D46 记录。
- 使用 flat C API：
  - `VR_InitInternal2(..., VRApplication_Overlay, ...)`
  - `VR_ShutdownInternal`
  - `VR_GetGenericInterface("FnTable:IVROverlay_028", ...)`
  - `VR_GetGenericInterface("FnTable:IVRSystem_026", ...)`
- 手写最小函数表前缀，并用 `offset_of!` 单测锁定关键函数偏移。
- 只封装需要的函数：
  - Overlay：`CreateOverlay`、`DestroyOverlay`、`FindOverlay`、
    `SetOverlayRaw`、`SetOverlayWidthInMeters`、`SetOverlayTexelAspect`、
    `SetOverlayTransformTrackedDeviceRelative`、`SetOverlayFlag`、
    `SetOverlayAlpha`、`SetOverlayInputMethod`、`SetOverlayMouseScale`、
    `ShowOverlay`、`HideOverlay`、`PollNextOverlayEvent`、`IsOverlayVisible`。
  - System：`GetTrackedDeviceIndexForControllerRole`、`GetControllerStateWithPose`
    （用于判断手柄是否连接/位姿是否有效；后续自定义指针也可复用）。
- 不采用 `openvr` crate：它会 CMake 静态编译 OpenVR，需要 CMake + libclang；
  与当前构建环境不匹配。若以后接受这些构建依赖，可重新评估。

### 4.2 手背锚定

- 默认 `TrackedControllerRole_RightHand`；左手 laser。
- `position = right_back` 时：

```rust
let index = system.GetTrackedDeviceIndexForControllerRole(TrackedControllerRole_RightHand);
overlay.SetOverlayTransformTrackedDeviceRelative(handle, index, &right_hand_to_overlay);
```

- `hand_to_overlay` 是“手柄局部坐标系 → 面板”的常量矩阵，必须用真实手柄标定；
  不能照搬 HMD/世界坐标的矩阵。默认矩阵按 **Quest Touch** 标定（当前目标设备）。
- 面板悬浮在手背上方约 10–20cm，朝向脸部；具体 offset/rotation 由校准工具确定。
- 手柄未连接 / 位姿无效：隐藏 overlay，状态「等待手柄」；恢复后重新挂载。
- 不支持纯手部追踪；页面可提示「需要手柄」，但不写激光手提示。
- 不再有 HeadLocked / WorldFixed 分支。`overlay_debug --anchor front` 只是开发期绝对变换，
  不进入产品行为。

### 4.3 渲染

- 把 `bubble` 从 `src/gui/bubble.rs` 抽到共享模块（如 `src/bubble.rs`），
  增加 `BubbleStyle`：
  - 实时页：scale = 1.0，显示外侧语言代码；
  - Overlay：scale ≈ 1.3，隐藏外侧语言代码（近距离 11pt 不可读）。
- 纹理 **1024×768（4:3）**；物理宽度 0.28m；高度约 0.21m。
- 最多 3 条气泡；未来加按钮时底部预留空间。
- 离屏 egui：
  1. 自己构造 `egui::RawInput`；
  2. `ctx.run(raw_input, |ctx| overlay_ui(...))`；
  3. `ctx.tessellate(...)`；
  4. 应用 `TexturesDelta` 到 `dream-soft-render` 的纹理；
  5. 把 `ClippedPrimitive::Mesh` 转成 `Frame::Mesh`（顶点/UV/颜色/纹理 ID/clip rect）；
  6. 黑/白两遍渲染，恢复 premultiplied alpha；
  7. `SetOverlayRaw(handle, ptr, W, H, 4)`，设置 `VROverlayFlags_IsPremultiplied`。
- 不使用 `render_egui`；不用 GPU 纹理共享。
- 纹理外透明；内部半透明深色圆角底板，保证近距可读。
- 初版不显示外侧语言代码；翻译模式仍显示原文小字行（必要时后续调整字号）。

### 4.4 事件与数据流

```text
session::emit ──(clone)──▶ GUI 事件通道
             ├──(clone)──▶ ChatboxService
             └──(clone)──▶ OverlayService（Local + Others）

others_line::Emitter ──(clone)──▶ GUI + OverlayService

OverlayService
  ├─ Timeline（我方 + 对方，历史保留）
  ├─ Settings（enabled/position/width/max_bubbles/show_source/翻译模式）
  ├─ 100ms tick；有 dirty 时 10Hz 渲染
  ├─ 输入（初版空） → egui RawInput
  ├─ 渲染 → RGBA → SteamVR backend
  └─ status / preview
```

- 新增 overlay 事件接收端：
  - `pipeline::session::run` 增加 `Option<Sender<PipelineEvent>>`；
  - `pipeline::others_line::Params` 增加同样的 sender；`Emitter::send` 克隆给 overlay；
  - 否则只有我方气泡。
- `OverlayService` 维护自己的 `Timeline`，不依赖 GUI 重绘（D33 模式）。
- 会话开始/停止、线路关闭时，通过控制命令调用 `reset_open` / `drop_open`；
  不要只在 `StateChanged(Running)` 时重置，避免首句被清掉。
- 停止会话后保留历史气泡；`StateChanged(Idle/Failed)` 只清未完成消息。
- Overlay 设置由 `sync_from_config` 更新；`[overlay]` 不进指纹。
- 初版动作通道 `OverlayAction` 可先留空/不产生；按钮落地后再加变体。

### 4.5 输入（初版只预留）

初版：

- `SetOverlayInputMethod(None)`；
- 不设置 `MakeOverlaysInteractiveIfVisible`；
- `RawInput.events` 为空。

按钮落地时：

```text
SetOverlayInputMethod(Mouse)
SetOverlayMouseScale(W, H)
SetOverlayFlag(MakeOverlaysInteractiveIfVisible, true)
PollNextOverlayEvent(...)
  → VREvent_MouseMove / MouseButtonDown / MouseButtonUp / Scroll
  → GL 左下角坐标转 top-left：x * W, (1 - y) * H
  → egui::Event::PointerMoved / PointerButton / MouseWheel
  → Button → OverlayAction → mpsc → BivoxApp
```

- 默认右手面板 + 左手激光；系统激光选错手时再改自定义射线
  （`ComputeOverlayIntersection` + controller pose）。
- 文本输入后续用 `ShowKeyboardForOverlay` + `VREvent_KeyboardCharInput/Done`。
- 刷新率：只读 10 Hz；交互 20–30 Hz。
- 1024×768×4 ≈ 3.1 MB/帧；20 Hz 约 63 MB/s，测量 `SetOverlayRaw` 压力后再调。

---

## 5. 模块/文件计划

```text
src/bubble.rs                    # 从 gui/bubble.rs 抽出，加 BubbleStyle
src/overlay/mod.rs               # 公共类型：settings/status/action
src/overlay/render.rs            # 1024×768 场景 → premultiplied RGBA（两遍）
src/overlay/egui_bridge.rs       # RawInput、TexturesDelta、Mesh 转换
src/overlay/steamvr/mod.rs       # overlay 生命周期、手背锚定、输入轮询
src/overlay/steamvr/ffi.rs       # 动态加载 + IVROverlay_028 / IVRSystem_026
src/overlay/service.rs           # 独立线程、Timeline、设置、状态、预览
src/gui/steamvr_tab.rs           # 「SteamVR」页签
src/gui/overlay_preview.rs       # 棋盘格预览/合成点击（可拆）
src/bin/overlay_debug.rs         # 开发期 CLI，不随发布包分发
```

- `src/gui/bubble.rs` 移动后更新 `realtime_tab` 引用与模块文档。
- `gui/` 单文件 ≤300 行（CONTRIBUTING）；页签/预览拆文件。
- 新页签加入 `Tab::ALL`、`central`、错误映射；D38 顺序更新。
- `Cargo.toml` 加 `dream-soft-render = { version = "=2.0.0" }`：我们只用它的核心光栅化
  API（`Frame`/`Mesh`/`TextureId`），不使用它的 `egui` feature；egui 桥由我们自己的代码完成。
- `THIRD_PARTY_NOTICES.md` 增加 `dream-soft-render`（MIT/Apache）与必要的 OpenVR 许可说明。

---

## 6. 实施清单

### Phase 0：仓库外 spike

- [ ] 动态加载 `openvr_api.dll`，`VR_InitInternal2(VRApplication_Overlay)`。
- [ ] `CreateOverlay` + `SetOverlayFromFile` 静态 PNG，在 null 驱动 + Overlay Viewer 看到。
- [ ] 验证 `IVROverlay_028` / `IVRSystem_026` 接口版本可用。
- [ ] 验证 `SetOverlayRaw` 方向、alpha、premultiplied、颜色空间。

### Phase 1：渲染与输入桥

- [ ] 抽出 `bubble`，加 `BubbleStyle`；实时页行为不变。
- [ ] 实现 `egui_bridge`：RawInput、纹理 delta、Mesh 转换、clip rect。
- [ ] 实现两遍 alpha 恢复；测试角落 alpha=0、气泡中心 alpha>0。
- [ ] 1024×768、4:3、3 条气泡、scale 1.3、隐藏外侧语言代码。
- [ ] 用 `RawInput.events=[]` 先跑通。

### Phase 2：OpenVR 后端

- [ ] FFI 动态加载与 `openvrpaths.vrpath` 定位；失败状态。
- [ ] 函数表偏移单测。
- [ ] `CreateOverlay("bivox.chat")`、宽度/texel aspect/flags。
- [ ] `SetOverlayTransformTrackedDeviceRelative` 到手柄；默认右手。
- [ ] 手柄断开/恢复：隐藏/重新挂载；`GetTrackedDeviceIndexForControllerRole`。
- [ ] `SetOverlayRaw`、`ShowOverlay`/`HideOverlay`、退出清理。
- [ ] SteamVR 未运行/启动/退出时的重试与状态。

### Phase 3：服务与 App 接入

- [ ] `OverlayService` 独立线程、`Timeline`、settings、status、preview。
- [ ] session/others_line 事件 fan-out。
- [ ] `reset_open`/`drop_open` 控制命令。
- [ ] `BivoxApp` spawn/update/shutdown；关闭顺序。
- [ ] `[overlay]` 配置、校验、默认值、序列化。
- [ ] `config_signature` 排除 `[overlay]`，补测试。

### Phase 4：GUI

- [ ] 新「SteamVR」页签，放在 VRChat 右侧。
- [ ] 启用 checkbox、position 选择、宽度、最多气泡。
- [ ] 状态：未启用 / 等待 SteamVR / 等待手柄 / 已连接 / 显示中 / 错误。
- [ ] 桌面预览（棋盘格、真实 Timeline）。
- [ ] 不写“激光请用左手”提示。
- [ ] 页面宽度 1100px 下 10 个页签仍能放下；否则调整页签间距。

### Phase 5：调试工具

- [ ] `src/bin/overlay_debug.rs`：`--anchor front` 无手柄时的绝对变换，测 SteamVR 纹理/输入 API。
- [ ] `src/bin/overlay_debug.rs`：`--position right_back --tune` 键盘微调 offset/rotation，打印矩阵。
- [ ] 合成鼠标点击预览，测按钮命中（按钮阶段）。
- [ ] 日志：初始化、Overlay 错误、手柄索引/位姿、重试。

### Phase 6：文档与门禁

- [ ] 新增 D46（定稿后合并）。
- [ ] 更新 D38 页签顺序。
- [ ] 更新 D20 的例外列表（`[overlay]` 不进指纹）。
- [ ] 更新 D4（`openvrpaths.vrpath` 例外）。
- [ ] 更新 architecture：模块树、数据流、线程/生命周期、GUI 表。
- [ ] 更新 README：SteamVR 页、右手默认、需要手柄、无按钮初版、隐私提示。
- [ ] 更新 CONTRIBUTING：新增 `src/bin/overlay_debug.rs`（第五个 bin，不随发布包分发）。
- [ ] `cargo fmt/build --bins/test/clippy` 全绿；`git diff --check`。

---

## 7. 验证清单

### 自动化

- [ ] 配置解析/校验/序列化/默认值。
- [ ] `config_signature` 忽略 `[overlay]`。
- [ ] `Timeline` 我方+对方、历史、reset/drop open。
- [ ] 两遍 alpha：透明角、气泡中心、CJK/emoji 不 panic。
- [ ] FFI 函数表 offset。
- [ ] 服务状态机（用 fake backend）：禁用/启用/SteamVR 缺失/手柄缺失/退出重试。
- [ ] 事件 fan-out 回归：他人事件确实到 overlay。

### 桌面（无头显）

- [ ] `EGUI_INSPECTION=1 .\scripts\dev-run.ps1`，SteamVR 页开关/尺寸/预览正常。
- [ ] 预览显示真实 Timeline 的 3 条气泡、左右分侧、翻译原文行。
- [ ] 关闭/重开开关不触发会话重启。
- [ ] `overlay_debug --anchor front --png out.png` 输出正确。
- [ ] 合成点击能触发按钮动作（按钮阶段）。

### 无头显 SteamVR（null driver）

- [ ] `forcedDriver = "null"`、`driver_null.enable = true` 后，`overlay_debug --anchor front`
      能在 Overlay Viewer 看到纹理、alpha、方向。
- [ ] `overlay_debug --anchor front` 的系统激光/坐标映射（如启用输入）可初步测试。
- [ ] `vrserver.txt` / `vrcompositor.txt` 无 overlay 错误。

### 真机（Quest 3 + Virtual Desktop）

- [ ] 默认右手手背面板出现在手背上方 10–20cm，角度可读。
- [ ] 手柄断开/恢复时隐藏/重挂载。
- [ ] 左手激光能指向面板；坐标无上下颠倒/偏移。
- [ ] 系统激光是否抢 VRChat 输入/是否选错手。
- [ ] 3 条气泡在 0.28m 宽下的字号、换行、翻译原文行可读性。
- [ ] 停止会话后历史气泡保留；关窗后 overlay 消失。
- [ ] 运行中开关/改宽度不重启会话。
- [ ] 帧率/上传无明显卡顿。
- [ ] 隐私：SteamVR 镜像/串流会带上 overlay。

---

## 8. 风险与已确认项

### 风险

1. **右手面板 + 左手激光**：系统激光可能选错手/抢 VRChat 输入；不行就换自定义射线。
2. **手背矩阵**：不同手柄局部轴不同；必须真实手柄校准，不能只靠猜。
3. **近距可读性**：0.28m 宽、3 条气泡、原文/语言码偏小；先按 compact style 试。
4. **SetOverlayRaw 带宽**：1024×768 交互 20–30 Hz 约 63–94 MB/s；压力大再降分辨率/频率。
5. **手柄依赖**：无 controller pose 不支持；null 驱动无法完整测试手背姿态。
6. **`dream-soft-render` 较新**：钉死 2.0.0，封装在 render 后；两遍 alpha 是 workaround。
7. **FFI 版本**：`IVROverlay_028` / `IVRSystem_026`；SteamVR 更新需回归 offset。
8. **动作延迟**：按钮动作经 GUI 线程；GUI 最小化时可能延迟，后续需验证/改进。

### 已确认（2026-11-25）

- [x] `position` 当前支持 `right_back`（默认）和 `left_back`；两者都是手背。
- [x] `overlay_debug` 放在 `src/bin/overlay_debug.rs`，作为第五个开发期 bin，不随发布包分发。
- [x] 默认右手矩阵按 **Quest Touch** 标定。
- [x] 初版按 0.28m 宽、3 条气泡、1024×768 实现，先看实际效果再调。

---

## 9. 变更记录

- 2026-11-24：讨论定稿。确定只手背固定、默认右手、左手激光、0.28m/3 条、系统激光先行、
  输入就绪渲染桥、动作回传、桌面/无头显/真机调试方案。
- 2026-11-25：确认 `position` 支持 `right_back`（默认）/ `left_back`；`overlay_debug` 放
  `src/bin/overlay_debug.rs`；默认右手矩阵按 Quest Touch 标定；初版按 0.28m/3 条实现。
