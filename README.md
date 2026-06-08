# Arknights Cost Bar Ruler (明日方舟费用条尺子)

> Rust 重写版本 — 原生 Win32 高性能实现

一个为《明日方舟》设计的原生 Windows 悬浮窗工具，基于费用条变化测量当前关卡内的帧数，帮助玩家复刻极限操作。

## 特性

- **高性能**: 纯 Rust 实现，原生 Win32 窗口，极低延迟
- **自适应费用条速度变化**:
  - 校准系统可以轻松处理三减费、二减费等环境
  - 快速切换不同的费用校准文件
- **捕获后端**:
  - **MuMu 模拟器 12**: 通过命名管道高速截图
  - **雷电模拟器 9**: 截图增强模式
  - **Windows 桌面版**: 直接捕获明日方舟电脑版窗口
  - **通用 ADB**: 兼容其他模拟器（通过 PyO3 桥接）
- **原生悬浮窗**: 半透明、可拖动的 Win32 覆盖层，不遮挡游戏画面
- **DPI 感知**: 支持高 DPI 显示器，自动缩放
- **系统托盘**: 托盘图标提供完整的菜单控制
- **用户友好**:
  - 首次运行提供连接设置向导
  - 自动化的校准流程
  - 全局计时器和停表功能
  - 多语言支持（中文 / English）
- **WebSocket API**: 允许第三方工具获取帧数和计时器数据

## 项目结构

```
ArknightsCostBarRuler/
├── Cargo.toml              # 工作区根配置
├── crates/
│   ├── ruler-core/         # 核心库：截图、分析、引擎
│   │   ├── src/capture/    # 截图后端 (mumu, ldplayer, windows)
│   │   ├── src/analysis/   # 费用条分析 (校准、扫描、ROI)
│   │   ├── src/config.rs   # 配置管理
│   │   └── src/engine.rs   # 帧检测引擎
│   ├── ruler-app/          # 桌面应用：Win32 窗口、托盘、API 服务
│   │   └── src/
│   │       ├── overlay.rs  # 悬浮窗覆盖层
│   │       ├── tray.rs     # 系统托盘
│   │       ├── worker.rs   # 事件驱动工作线程
│   │       ├── api.rs      # WebSocket API
│   │       └── config_wizard.rs  # 连接设置向导
│   └── ruler-pyo3/         # PyO3 桥接（可选 Python 互操作）
├── icons/                  # UI 图标
├── ruler/locales/          # 本地化翻译文件
└── LICENSES/               # 第三方许可证
```

## 快速开始

### 从源码构建

**前置要求:**

- Rust 1.75+
- Windows 10/11（仅支持 Windows）

**构建:**

```bash
cargo build --release -p ruler-app
```

构建产物位于 `target/release/ruler-app.exe`。

### 运行

```bash
cargo run --release -p ruler-app
```

首次运行时会弹出连接设置向导，选择您的模拟器类型并完成配置。

### 首次校准

1. 程序启动后，右键托盘图标，选择"校准配置 > -- 新建 --"
2. 进入任意关卡
3. **点击一个地图上的干员（或可选中单位），让游戏进入慢速模式**
4. 点击悬浮窗左侧按钮开始校准
5. 校准完成后，悬浮窗将实时显示当前帧数

### 环境变量

- `RUST_LOG`: 日志级别，如 `info`、`debug`、`trace`（默认 `info`）

## WebSocket API

运行中的尺子实例在 `ws://localhost:9799` 提供 WebSocket API，允许第三方工具获取帧数和计时器数据。

## 注意

- 该程序在以下环境下不可用：
  - 部署费用已到达上限
  - 部署费用无法自然回复（如剿灭作战）
  - 部署费用自然回复被锁定（如第 15 章的"活性态萨卡兹术师结晶"）
- 如果程序运行异常，请尝试：
  - 删除 `config.json` 后重新配置
  - 以 `RUST_LOG=debug` 环境变量启动，查看诊断日志
  - 在 [Issue 页面](https://github.com/ZeroAd-06/ArknightsCostBarRuler/issues) 报告问题

## 许可 & 致谢

本项目在 `MIT License` 下开源。

本项目参考了以下优秀的开源项目：

- **[MaaFramework](https://github.com/MaaXYZ/MaaFramework)**: MuMu 模拟器 12 和 雷电模拟器 9 截图增强功能的适配参考了其实现。
- **[Minicap](https://github.com/DeviceFarmer/minicap)**: 通用安卓屏幕高速截图方案。
- **[Google Material Symbols](https://fonts.google.com/icons)**: 本项目使用的图标资源。

您可以在本仓库的 `LICENSES` 文件夹中找到相关协议的副本。

---

> 原 Python 版本在 `master` 分支维护。
