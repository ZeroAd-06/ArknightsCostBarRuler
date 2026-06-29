# 三层架构

从 v2.2 起，尺子内部重构为三层架构，以获得更好的可扩展性。终端用户的体验和性能没有变化——UI、计时器、校准、API 全部和以前一样工作。重构的目的是让未来可以挂载更多分析消费者 (录像、统计、外部插件等) 而不需要改动主循环。

## 架构总览

```
┌─────────────────────────────────────────────────────────┐
│  Layer 1: 连接 + 截图层 (CapturePipeline)               │
│                                                         │
│  ┌─────────────┐    ┌──────────────┐    ┌────────────┐  │
│  │ CaptureBackend │  │  FrameStore  │    │ PipeServer │  │
│  │ (MuMu/Adb/   │───▶│ (内存+磁盘溢出)│───▶│ (Windows   │  │
│  │  LDPlayer/Win)   │  │              │    │  named pipe)│  │
│  └─────────────┘    └──────────────┘    └─────┬──────┘  │
│                                                │         │
│  独立线程持续截图，分配单调递增的 frame_id，     │         │
│  推入 FrameStore，通过 named pipe 分发给消费者  │         │
└────────────────────────────────────────────────┼─────────┘
                                                 │
                    ┌────────────────────────────┼────────┐
                    │                            │        │
                    ▼                            ▼        ▼
┌───────────────────────────┐  ┌──────────────────────┐  ┌──────────────────┐
│ Layer 2: 分析层            │  │ DebugRecorder 消费者  │  │ Calibration 消费者│
│ (AnalyzerConsumer)         │  │ (InOrder)             │  │ (InOrder, 临时)   │
│                            │  │                       │  │                  │
│ 策略: SkipToLatest         │  │ 策略: InOrder         │  │ 策略: InOrder    │
│ 游标始终跳到最新帧          │  │ 按序处理每一帧         │  │ 按序处理每一帧    │
│                            │  │                       │  │                  │
│ 职责:                       │  │ 职责:                 │  │ 职责:            │
│ - 检测战斗状态              │  │ - 录制视频 (MKV)       │  │ - 采集像素宽度    │
│ - 查询校准表                │  │ - 录制分析 CSV         │  │ - 采集校准周期    │
│ - 计算 FrameResult          │  │                       │  │                  │
│ - 发布到 SharedAppState     │  │                       │  │                  │
└─────────────┬──────────────┘  └──────────────────────┘  └──────────────────┘
              │
              ▼
┌─────────────────────────────────────────────────────────┐
│  Layer 3: UI 层                                          │
│                                                         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────┐  │
│  │ Overlay 窗口 │  │ API 服务器   │  │ (未来消费者)     │  │
│  │ (Slint HUD) │  │ (WS + HTTP) │  │                 │  │
│  └──────┬──────┘  └──────┬──────┘  └─────────────────┘  │
│         │                │                               │
│         └────────────────┘                               │
│              ▲                                           │
│              │                                           │
│         SharedAppState (Arc<Mutex<AppStateSnapshot>>)    │
│         分析层写入，UI 层读取                              │
└─────────────────────────────────────────────────────────┘
```

## 各层职责

### Layer 1: 连接 + 截图层 (`ruler-core::pipeline`)

- **CapturePipeline** 拥有 capture backend (MuMu / Adb / LDPlayer / Windows / Replay)，在独立线程中持续截图
- 每帧分配单调递增的 `frame_id`，推入 **FrameStore**
- **FrameStore** 在内存中保存帧，当总内存超过 512 MiB 时自动将最旧的仍需保留的帧溢出到磁盘 (`{log_session_dir}/frame_spill/frame_{id:020}.bin`)，按需读回
- **PipeServer** 在 Windows named pipe (`\\.\pipe\ruler-frames-{session}`) 上接受消费者连接
- 每个消费者有自己的游标 (forward-only)，当所有消费者的游标都越过某帧时，该帧被释放
- 帧通过 named pipe 分发给所有消费者 (包括进程内的分析层、DebugRecorder、Calibration)

### Layer 2: 分析层 (`ruler-app::analyzer_consumer`)

- **AnalyzerConsumer** 注册为 Layer 1 的一个 `SkipToLatest` 消费者
- 游标始终指向最新帧——如果分析速度跟不上截图速度，中间帧被丢弃，保证显示的计时器始终反映最新画面
- 取出帧后调用 **Analyzer** (来自 `ruler-core::engine`) 进行:
  - 战斗状态检测 (OneXRunning / TwoXRunning / BattleBegin / ...)
  - 费用条像素宽度查询
  - 校准表反查 → 逻辑帧
  - 周期计数、负费用检测、计时器累计
- 分析结果 (`FrameResult`) 发布到 `SharedAppState`，供 Layer 3 读取
- 通过命令通道 (`AnalyzerCommand`) 接收 worker 的控制指令 (加载校准、重置计时器、切换显示模式等)

### Layer 3: UI 层 (`ruler-app::overlay` + `ruler-app::api`)

- **Overlay 窗口** (Slint HUD) 从 `SharedAppState` 读取 UI 快照，渲染计时器、帧数、进度条
- **API 服务器** (WebSocket + HTTP) 从 `SharedAppState` 读取分析结果，推送给外部工具
- UI 层不直接访问 capture backend 或 Analyzer——它只消费 `SharedAppState` 中的数据
- 这层的接口和之前完全一致，外部工具 (对轴器等) 不需要任何改动

## 消费者策略

| 策略 | 游标行为 | 适用场景 |
|------|---------|---------|
| `InOrder` | 按序处理每一帧，不丢帧 | 录像、标定、外部统计 |
| `SkipToLatest` | 始终跳到最新帧，丢弃中间帧 | 实时分析 (L2) |

`InOrder` 消费者如果处理速度慢于截图速度，帧会在 FrameStore 中积压，超阈值后溢出到磁盘。`SkipToLatest` 消费者永远不会积压——它只看到最新帧。

## Named Pipe 线协议

外部消费者 (如 Python 插件) 可以直接连接 named pipe 获取帧流。协议是二进制的，所有整数使用小端序。

### 管道名称

```
\\.\pipe\ruler-frames-{session_id}
```

`session_id` 是每次启动时生成的唯一标识 (纳秒时间戳)。可以通过 API 或日志获取当前 session 的管道名。

### 数据包格式

**Subscribe** (客户端 → 服务端，连接后发送一次):
```
[4]  magic = b"RSB1"
[1]  policy (0 = InOrder, 1 = SkipToLatest)
[8]  start_frame_id (u64) — 消费者的初始游标
```

**Frame** (服务端 → 客户端):
```
[4]  magic = b"RFM1"
[8]  frame_id (u64)
[4]  width (u32)
[4]  height (u32)
[4]  format_tag (u32) — 0=RGBA, 1=BGR, 2=BGRA
[8]  capture_duration_us (u64)
[8]  capture_timestamp_ns (u64)
[8]  data_len (u64)
[data_len] 像素数据
```

**Pull** (客户端 → 服务端，仅 SkipToLatest):
```
[4]  magic = b"RPUL"
```
服务端响应最新帧 (RFM1) 或在关闭时响应 RNON。

**Ack** (客户端 → 服务端，仅 InOrder):
```
[4]  magic = b"RACK"
[8]  frame_id (u64) — 已处理完的最后一帧
```

**Error** (服务端 → 客户端):
```
[4]  magic = b"RERR"
[2]  message_len (u16)
[message_len] UTF-8 错误消息
```

### Python 参考客户端

```python
import struct, win32file

pipe_name = r"\\.\pipe\ruler-frames-1234567890"
handle = win32file.CreateFileW(
    pipe_name,
    win32file.GENERIC_READ | win32file.GENERIC_WRITE,
    0, None, win32file.OPEN_EXISTING, 0, None,
)

# Subscribe (InOrder, start from frame 0)
win32file.WriteFile(handle, b"RSB1" + bytes([0]) + struct.pack("<Q", 0))

while True:
    # Read frame header
    header = win32file.ReadFile(handle, 4 + 8 + 4*3 + 8*3)[1]
    magic = header[:4]
    assert magic == b"RFM1", f"unexpected magic: {magic}"
    (frame_id, width, height, fmt_tag,
     cap_us, cap_ns, data_len) = struct.unpack("<QIIIQQQ", header[4:])
    # Read pixel data
    _, data = win32file.ReadFile(handle, data_len)
    # Process frame...
    # Ack (InOrder only)
    win32file.WriteFile(handle, b"RACK" + struct.pack("<Q", frame_id))
```

## 添加新的分析消费者

未来要挂载更多分析消费者，只需:

1. 调用 `pipeline.connect_consumer(ConsumerPolicy::InOrder, 0)` 获取一个 `ConsumerPipe`
2. 在新线程中循环 `pipe.recv_frame()` → 处理 → `pipe.ack(frame.id)`
3. 处理完成后 drop `ConsumerPipe` 即可断开连接

进程内消费者和外部消费者使用完全相同的协议，只是连接方式不同 (进程内直接 `CreateFileW` 打开管道名)。

## 性能说明

- 截图线程与分析线程解耦，分析慢不会拖慢截图
- `SkipToLatest` 策略保证分析层始终处理最新帧，延迟 ≤ 一帧
- `InOrder` 消费者在内存不足时自动溢出到磁盘，不会 OOM
- named pipe 在同进程间有内核优化 (section object)，实际吞吐接近零拷贝
- 512 MiB 内存阈值足以缓存约 5 秒的 1440p RGBA 帧 (约 100 MB/帧)
