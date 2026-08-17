# AGENTS.md

winflow — 一个用 Rust 编写的 macOS 窗口切换器，用来补强系统自带的
Cmd+Tab：带实时窗口缩略图、按桌面（Space）分组、LRU 排序、以及
hjkl / 方向键 / 鼠标 多种切换方式。

> 任何后续开发、重构、修复都必须遵守本文件中的设计约束。

## 一、核心产品约束（来自产品需求，不可违背）

1. **两种模式**
   - 模式 1（`⌘Tab`，固定）：当前桌面（Space）的**所有活跃窗口**切换，
     **每窗口一条目**——同一程序的多个窗口都显示（例如多个 VSCode /
     Chrome 窗口）。
   - 模式 2（`⌘\``，固定）：当前前台程序的窗口切换（模式 1 的子集），
     每窗口一条目。
   - **快捷键固定且覆盖系统**：HID 层事件 tap 必须吞掉 `⌘Tab`/`⌘\`` 的
     keydown，阻止系统切换器与系统窗口循环出现。`⌘⇧Tab` 打开模式 1
     （向后）。禁止把热键改回可配置。
2. **每个桌面是一个空间，且按显示器独立**。唤醒切换器时，只展示**按下
   快捷键所在显示器**当前 Space 上的窗口（叠加层也出现在该显示器上）；
   每个显示器的每个桌面都是独立世界，跨显示器/跨 Space 的窗口不出现。
   叠加层打开期间若该显示器桌面切换（窗口集合变化），自动刷新列表。
3. **缩略图等高、不等宽**：每个窗口缩略图高度一致，宽度按窗口实际
   宽高比计算，因此一行可以放多个窗口。
4. **边界环绕导航**：方向键/hjkl 移动到网格上下左右边缘后，继续按键会
   从另一侧循环（如最左边再按左 → 跳到最右边）。
5. **异常窗口过滤**：模式 1/2 都逐窗口展示，因此必须过滤掉异常窗口，
   例如飞书的水印层（无标题、被同程序更大窗口完全包含、alpha < 1、
   尺寸远小于父窗口）。过滤规则见 `src/windows.rs::sanitize`。
   **保底规则**：有基本合法窗口的程序至少保留一个主窗口——同界去重只
   在至少一方是无标题叠加层时生效（飞书全屏主窗与其无标题子层同界，
   无标题的反而可能在前，保留有标题的）；**两个都有标题的同界窗口
   （如同一显示器上两个最大化 VSCode 窗口）都保留**。若某程序所有窗口
   都被叠加层启发式过滤掉，则重新加回其主窗口（最靠前的有标题窗口，
   否则最靠前窗口），避免整个程序消失。
6. **MRU 切换逻辑**：维护 `Core::mru`（按最近激活排序）+ `Core::active`
   （当前激活窗口）+ `Core::prev_window`（正在离开的窗口）。
   打开叠加层时列表按 `[prev] [active] [mru 其余] [CG 序其余]` 排列，
   选中 `prev`（从 A 切到 B 后再次唤起，默认选中 A，再按一次回 B）。
   外部切换（点击其他窗口）在 show 时同步进 `active`/`mru`。
7. **快速切换（可配置延迟）+ 叠加层随 ⌘ 显隐**：热键按下不立即弹叠加层——
   在 `quick_delay_ms`（默认 80ms，面板可调）内松开修饰键 → `QuickSwitch`
   直接激活 `prev_window`（不弹界面）；超过延迟仍按住 ⌘ → 弹叠加层，
   **且只要不松开 ⌘ 就一直展示**（即使没有选择任何窗口）。
   **松开 ⌘ 自动切换到选中框选中的窗口并关闭叠加层**（无需回车/点选
   显式确认），下次按下重新唤起；回车/鼠标点选仍是显式切换方式。状态机：`quick_pending` +
   `quick_show_dispatched` + `cmd_held`（tap 线程在 flags 变化时写入；
   timer 只在 ⌘ 仍按住时派发 Show；重复 Tab 不重新 arm）。
8. **后台定时截图**：后台线程按间隔（默认 **45s**）定时截取活跃窗口的
   窗口图，缓存到共享内存，唤醒时优先用缓存（秒开），并异步刷新。
   **启动即预热**：`main()` 在启动时把当前可见窗口写入 `core.tracked`
   并置 `refresh_all`，调度线程首轮（~50ms）立即截取；叠加层未打开期间
   每 ~10s 重同步 `tracked` 到屏幕上的窗口，保证首次唤出不空白、不卡顿。
   间隔可在菜单栏「配置…」面板调整（1–3600s），**持久化到
   `~/Library/Application Support/winflow/settings.conf`，重启生效保持**。
9. **多种切换输入**：Tab（下一个）/ Shift+Tab（上一个）、hjkl、方向键、
   滚轮、鼠标悬停即选中（选中框跟随鼠标，与键盘选择等价，无动画）+ 
   鼠标点选、Esc 取消、回车确认。**叠加层的展现与否严格由 ⌘ 按下状态
   决定**：按住展示、松开即**切换到选中项并关闭**（事件 tap 的 flags
   变化处理），不再有"失焦自动关闭"逻辑。
10. **菜单栏只保留两项**：「配置…」（设置面板）与「退出 winflow」。
   禁止再添加切换器菜单项或"打开配置文件"项。设置通过面板修改，
   面板确定的两个值（截图间隔、快捷判定延迟）持久化到
   `~/Library/Application Support/winflow/settings.conf`（`key=value`
   文本，无 serde），启动时 `config::load()` 合并到默认值。
11. **启动权限校验**：每次启动时在 `main()` 里调用
    `permissions::prompt_if_missing()` 自动检测辅助功能与屏幕录制权限；
    任一缺失即弹 `NSAlert` 弹窗，按钮跳转对应系统设置页。必须常驻，
    禁止静默降级不提示。

## 二、工程约束（性能 & 体积）

- **极致的性能与低占用**：release 二进制 ~570KB，无任何大框架依赖；
  常驻 RSS ~30MB（其中绝大部分是 AppKit 运行时基线，winflow 自身
  的堆内存很小，缩略图缓存按需裁剪）。
- 依赖面**保持最小**：`objc2` + `objc2-foundation` + `objc2-app-kit` +
  `block2`。禁止引入 tokio/async、GUI 框架、serde 等重依赖。
- **线程模型**（严格）：
  - 主线程：NSApplication run loop，拥有全部 UI（panel、渲染、激活）。
  - 事件 tap 线程：全局 `CGEventTap`（HID 层），处理全部键盘逻辑，
    只修改 `Core`（Mutex 保护，不包含 AppKit 对象，因此 Send）。
  - 截图线程：调度线程 + 2 个 worker，写入 `ThumbCache`
    （`Arc<RwLock<HashMap<u32, Thumb>>>`）。
  - 后台线程 → 主线程的命令通过 `CMD_QUEUE` + 主 run loop 上的
    `CFRunLoopTimer`（每 100ms 排空）投递。**禁止在后台线程碰任何
    AppKit 对象**。
  - 叠加层动画：无（选中框跟随鼠标/键盘即时切换，不做滑动动画）。
- **不要用 libdispatch**：本机 SDK 的 libdispatch.tbd 只声明了 arm64e，
  普通 arm64 链接会失败（`dispatch_get_main_queue` 等符号无法链接）。
  `CFRunLoopSource` 在 macOS 26 上经 FFI 创建会在
  `CFRunLoopAddSource` 时被异常触发，也不要使用；统一走
  `CFRunLoopTimer` + 互斥队列。
- **FFI 回调禁止 panic**：所有 `extern "C"` 回调（timer、event tap）
  内用 `catch_unwind` 包裹，防止 msg_send 校验 panic 导致整个进程 abort。
- **缩略图内存**：截图后立即用 CGContext 缩放（不要翻转 Y 轴——
  `CGWindowListCreateImage` 的输出对 `CGBitmapContextCreateImage` 读回
  已是正向，加 `translate(0,h)+scale(1,-1)` 会把缩略图上下颠倒，
  曾致缩略图倒置 bug，已移除），只存缩略高度像素（高度 =
  `thumb_height` × 显示器 backing scale，默认 200pt × 2 = 400px，
  1:1 显示不模糊；`thumb_px_scale` 为额外清晰度倍率，默认 1.0），
  缩放用 `kCGInterpolationHigh`，原始大图立即释放。
  NSImage 缓存只在主线程持有（`thumb_ns`，按 gen 失效）。
- **ObjC 调用注意事项**：`msg_send!` 的多参数 selector 必须用冒号形式
  并在参数间加逗号（`initWithContentRect: rect, styleMask: ...`）；
  `setActivationPolicy:` / `activateWithOptions:` 实际返回 BOOL；
  `kCGWindowIsOnscreen` 是 CFBoolean 不是 CFNumber；Rust 2021 闭包
  对 `ptr.0` 这类字段访问会做 disjoint capture（导致 raw pointer 无法
  Send），跨线程移动指针时用 `ThreadPtr::get()` 之类的封装方法访问。

## 三、架构模块（src/）

- `ffi.rs` — 手写 FFI：CoreGraphics（窗口列表/截图/事件 tap/CGS Space
  SPI）、CoreFoundation（CF 工具）、ApplicationServices（AX）、
  CFRunLoopTimer、常量与 keycode 表。
- `windows.rs` — 窗口枚举、过滤（sanitize：水印/重复/噪音）、
  模式 1 全窗口收集（每窗口一条目）+ 模式 2 前台程序子集、
  AX raise/focus、App 激活回退。
- `capture.rs` — 后台截图调度（启动预热 + 空闲时重同步 tracked）+ worker
  池 + `ThumbCache`。
- `layout.rs` — 纯数学网格布局（等高/变宽、行打包、边界环绕导航、
  命中测试）。坐标一律 bottom-left（AppKit 风格）。
- `state.rs` — `Core`（共享状态、LRU、模式）、`MainCmd` 命令、
  事件 tap、tick 线程、主线程命令队列（CFRunLoopTimer）。
- `overlay.rs` — 主线程 UI：无边框透明 NSWindow + NSImageView，
  每帧合成一张 NSImage（compose），鼠标事件本地监听，show/hide/
  activate/quick_switch 流程，权限检查；叠加层显隐由事件 tap 的
  ⌘ flags 变化驱动（按住展示、松开关闭），无失焦宽限期。
- `permissions.rs` — 启动权限校验：`check()` 检测 AX/屏幕录制，
  `prompt_if_missing()` 弹 `NSAlert`（主线程、`runModal`），按钮跳转
  系统设置页；`--force-perm-dialog` 可强制弹窗用于 UI 验证。
- `config.rs` — 内存态 `Config`（默认值）+ 持久化：`load()` 从
  `settings.conf` 读回两个面板可调项，`save()` 写回（路径可被
  `WINFLOW_CONFIG_FILE` 覆盖，供测试）。
- `menubar.rs` 配置面板：`NSStatusItem` + `NSMenu`（define_class! action
  target）+ `NSWindow`/`NSTextField`/`NSButton` 设置面板。注意：面板创建
  代码运行在命令处理器持有 `APP` 互斥锁的上下文内，**读取/写入运行时
  设置必须走无锁镜像（`CAPTURE_INTERVAL_MS` / `SHARED`），严禁在面板
  代码里调用 `with_app`（std Mutex 不可重入，会死锁）**。

## 四、关键不变式

- 条目粒度：模式 1 = 当前 Space 的每个活跃窗口一条目（不做每程序
  归并）；模式 2 = 前台程序的窗口子集。sanitize 负责剔除水印层/
  同界重复/噪音窗口。
- 选中状态统一：键盘导航、滚轮、鼠标悬停都改同一个 `Core::selection`；
  选中框即时显示在选中缩略图上（无动画）。
- 按显示器过滤：`collect` 只保留中心落在目标显示器 bounds（全局 CG 坐标）
  内的窗口；`CGWindowList` 本身是 on-screen-only，因此每个显示器的当前
  桌面即为其可见窗口集。叠加层打开期间用窗口 id 指纹检测桌面切换。
  （每显示器 Space SPI `CGSGetDisplayActiveSpace` 在 macOS 26 上不存在，
  故不用 space id 过滤，改为 bounds + 指纹。）
- LRU：`lru` 队首 = 最近离开的窗口（下次唤醒的选中项）；
  `prev_window` = 最近一次激活的窗口。
- 权限缺失降级：无屏幕录制 → 缩略图为占位、标题为空（此时关闭"空标题
  即水印"启发式）；无辅助功能 → AX raise/focus 禁用，回退
  `activateWithOptions(ActivateAllWindows)`。
- 叠加层失焦处理：无。叠加层随 ⌘ 按下/松开显隐（事件 tap flags 变化
  驱动）：松开 ⌘ 自动激活选中项并关闭；回车/点选为显式切换，
  点击空白/Esc 为取消。
- `initWithCGImage:size:` 会 retain CGImage（已验证 2→3），缩略图所有权：
  capture 线程持 +1，主线程只传借用引用，勿转移所有权（曾致双重释放崩溃）。

## 五、v1 已知限制（可后续增强）

- 模式 2 只显示当前 Space 上前台程序的窗口；跨 Space 窗口不显示，
  也不做程序化跨 Space 切换。
- 不显示最小化窗口（CGWindowList on-screen only）。
- 缩略图只在窗口内容变化后按 `capture_interval` 刷新，不监听窗口
  content 变化事件。
- 叠加层显示在按下快捷键的显示器上（按光标所在显示器确定，见 `ffi::cursor_display`），
  居中于该显示器。
- 设置持久化：面板修改的截图间隔/快捷判定延迟保存到
  `~/Library/Application Support/winflow/settings.conf`，重启保持。
