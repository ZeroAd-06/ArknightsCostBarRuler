# 本地 API 文档

你好呀！这篇文档是给想跟"明日方舟费用条尺子"联动、写自己工具的人看的。

## 概述

尺子运行时会在本地起一个小服务，把它分析出来的游戏状态实时对外提供。任何能发 HTTP 请求或连 WebSocket 的程序都能拿到这些数据，从而跟尺子联动 (之前的对轴器就是这么干的)。

服务同时提供两种取数方式，返回的字段完全一致，按你的场景挑一个用就行:

- **WebSocket** —— 服务器主动推送。连上就行，状态有变化时自动收到新数据，适合需要持续跟随的场景。
- **HTTP 快照** —— 一次性 `GET`，拿到当前这一刻的状态。适合偶尔查一下、不想常驻连接的场景。

- **数据格式:** JSON (UTF-8)
- **地址:** `127.0.0.1:2606`
  - WebSocket: `ws://127.0.0.1:2606/`
  - HTTP: `http://127.0.0.1:2606/`

> 端口固定是 `2606` (TCP)。

## 数据包格式

无论走 WebSocket 还是 HTTP，每条消息都是一个 JSON 对象，包含以下字段:

| 键 (Key)             | 类型 (Type)         | 描述                                                                                     | 示例 (运行中) | 示例 (空闲) |
| -------------------- | ------------------- | ---------------------------------------------------------------------------------------- | ------------- | ----------- |
| `isRunning`          | `boolean`           | 尺子当前是否识别到有效的费用条。为 `false` 表示费用条可能被锁、不存在或费用已满。        | `true`        | `false`     |
| `currentFrame`       | `integer` 或 `null` | 当前费用循环内的逻辑帧 (从 0 开始)。`isRunning` 为 `false` 时是 `null`。                  | `15`          | `null`      |
| `totalFramesInCycle` | `integer`           | 当前校准配置下，一次费用回复循环的总帧数。`isRunning` 为 `false` 时是 `0`。              | `30`          | `0`         |
| `totalElapsedFrames` | `integer`           | 从当前配置启动起累计经过的总逻辑帧，用来驱动 `分:秒:帧` 计时器。`isRunning` 为 `false` 时保持在上一个有效值。 | `75`          | `75`        |
| `activeProfile`      | `string` 或 `null`  | 当前加载的校准配置名 (基础名)。没加载配置时是 `null`。                                    | `"正常回费"`  | `null`      |

> `currentFrame` 的取值范围跟尺子界面上的"帧数显示模式" (`0/n-1` 等) 无关——API 始终返回从 `0` 开始的原始逻辑帧，界面上的不同数法只是显示层的换算。

### 消息示例

正常运行时:

```json
{
    "isRunning": true,
    "currentFrame": 15,
    "totalFramesInCycle": 30,
    "totalElapsedFrames": 75,
    "activeProfile": "正常回费"
}
```

没检测到费用条时:

```json
{
    "isRunning": false,
    "currentFrame": null,
    "totalFramesInCycle": 0,
    "totalElapsedFrames": 75,
    "activeProfile": "正常回费"
}
```

## WebSocket 用法

- 连上之后，服务器会**立刻推送一次**当前状态作为初始快照。
- 之后只在状态**发生变化**时才推送 (相同内容不会重复发)，所以收到一条就更新一次界面即可。
- 建议实现**断线重连**:尺子重启后，客户端能自己接回来。

一个最小的 Python 例子 (需要 `websockets` 库):

```python
import asyncio
import json
import websockets

async def main():
    async for ws in websockets.connect("ws://127.0.0.1:2606/"):
        try:
            async for message in ws:
                data = json.loads(message)
                print(data)
        except websockets.ConnectionClosed:
            continue  # 断了就重连

asyncio.run(main())
```

## HTTP 快照用法

直接 `GET http://127.0.0.1:2606/` 就能拿到当前状态的 JSON。响应带了 `Access-Control-Allow-Origin: *`，所以网页里用 `fetch` 跨域取用也没问题。

```bash
curl http://127.0.0.1:2606/
```

```javascript
const data = await fetch("http://127.0.0.1:2606/").then(r => r.json());
console.log(data.currentFrame, data.totalFramesInCycle);
```

## 版本与兼容性

本文档对应 **Rust 重写版 (v2.0)**。相比 Python 版，端口仍是 `2606`、字段保持不变，新增了 HTTP 快照这个取数方式。

往后的更新可能会在 JSON 里**新增**字段，但会尽量保证现有字段的**向后兼容**。
