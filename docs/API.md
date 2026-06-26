# 本地 API 文档

你好呀！这篇文档是给想跟“明日方舟费用条尺子”联动、写自己工具的人看的。

## 概述

尺子运行时会在本机 `127.0.0.1:2606` 起一个小服务，把当前分析状态、最近已分析帧和一组控制命令暴露给外部工具。

- **WebSocket:** `ws://127.0.0.1:2606/`
- **HTTP 快照:** `http://127.0.0.1:2606/`
- **数据格式:** JSON (UTF-8)
- **API 版本:** `apiVersion = 2`

兼容策略很简单：旧客户端原来读取的顶层字段仍然保留，服务器主动推送的 WebSocket 消息也仍然是顶层快照对象；新版只新增字段，并允许客户端通过 WebSocket 发送请求和控制命令。

> 写命令没有 token，也不做危险操作屏蔽。服务只绑定 `127.0.0.1`，默认信任本机进程；如果你运行了不可信脚本或网页自动化，请把它视为“可以操作尺子、删除校准、退出程序”的本机明文权限面。

## 快照字段

HTTP `GET /` 和 WebSocket 主动推送都返回同一种快照对象。旧字段继续在顶层存在：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `isRunning` | `boolean` | 当前是否识别到有效费用条。 |
| `currentFrame` | `integer \| null` | 当前费用循环内的原始逻辑帧，从 `0` 开始；与界面显示模式无关。 |
| `totalFramesInCycle` | `integer` | 当前费用回复循环总帧数；边界周期可能是基础周期 `N+1`。 |
| `totalElapsedFrames` | `integer` | 当前战斗内累计逻辑帧，用来驱动 `分:秒:帧` 计时器。 |
| `activeProfile` | `string \| null` | 当前加载的校准配置基础名。 |

新增字段如下：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `apiVersion` | `integer` | 本地 API 协议版本，当前为 `2`。 |
| `appVersion` | `string` | 尺子程序版本，例如 `v2.1.0`。 |
| `frameId` | `integer \| null` | Layer 1 截图管线的 `Frame.id`；不是逻辑费用帧，也不是 `sampleIndex`。 |
| `sampleIndex` | `integer` | analyzer 消费到的样本序号。 |
| `droppedSincePrevious` | `integer` | 与上一条已记录分析帧之间跳过的截图管线帧数。 |
| `rawPixelWidth` | `integer \| null` | 分析器看到的原始费用条宽度。 |
| `costIsNegative` | `boolean` | 当前是否处于负费显示。 |
| `battleState` | `string \| null` | 战斗状态识别结果。 |
| `captureWidth` / `captureHeight` | `integer \| null` | 本帧截图尺寸。 |
| `captureFormat` | `string \| null` | 本帧像素格式，目前常见为 `rgba`、`bgr` 或 `bgra`。 |
| `captureTimestampNs` | `integer \| null` | 截图时间戳，单位纳秒。 |
| `captureDurationUs` | `integer \| null` | 截图耗时，单位微秒。 |
| `cursorBlocked` | `boolean` | PC 自绘光标是否遮挡费用条；被遮挡帧不会写入历史分析记录。 |
| `displayMode` | `string` | 界面帧数显示模式：`0_to_n-1`、`0_to_n`、`1_to_n`。 |
| `displayFrame` / `displayTotal` | `string` | 已按界面显示模式格式化后的当前帧和总帧数。 |
| `time` | `string` | 已格式化计时器，格式为 `MM:SS:FF`。 |
| `lapFrames` | `integer \| null` | 当前 lap 计时帧数；未开启时为 `null`。 |
| `canUndoReset` | `boolean` | 是否可以撤销上一次计时器重置。 |
| `profiles` | `array` | 校准配置列表，每项包含 `filename`、`basename`、`totalFrames`、`resolution`、`isActive`。 |
| `historyOldestFrameId` / `historyLatestFrameId` | `integer \| null` | 当前战斗保留的最早 / 最新已分析历史帧。 |

示例：

```json
{
  "apiVersion": 2,
  "appVersion": "v2.1.0",
  "isRunning": true,
  "currentFrame": 15,
  "totalFramesInCycle": 30,
  "totalElapsedFrames": 75,
  "activeProfile": "正常回费",
  "frameId": 4242,
  "sampleIndex": 311,
  "droppedSincePrevious": 2,
  "rawPixelWidth": 123,
  "costIsNegative": false,
  "battleState": "battle",
  "captureWidth": 1280,
  "captureHeight": 720,
  "captureFormat": "rgba",
  "captureTimestampNs": 1234567890,
  "captureDurationUs": 1500,
  "cursorBlocked": false,
  "displayMode": "0_to_n-1",
  "displayFrame": "15",
  "displayTotal": "/29",
  "time": "00:02:15",
  "lapFrames": null,
  "canUndoReset": true,
  "profiles": [],
  "historyOldestFrameId": 4200,
  "historyLatestFrameId": 4242
}
```

## WebSocket 请求

客户端可以继续只读服务器推送；也可以向同一个 WebSocket 连接发送 JSON 请求。响应统一使用 typed envelope，并回显可选 `requestId`：

```json
{"type":"ack","requestId":"r1","action":"adjustTimer"}
{"type":"snapshot","requestId":"r2","payload":{ "...": "快照字段" }}
{"type":"frame","requestId":"r3","requestedFrameId":4241,"actualFrameId":4240,"fellBack":true,"fallbackReason":"frame_skipped","frame":{ "...": "历史帧字段" }}
{"type":"error","requestId":"r4","code":"invalid_request","message":"..."}
```

支持的请求：

| `type` | 参数 | 说明 |
| --- | --- | --- |
| `getSnapshot` | 无 | 返回当前快照。 |
| `getFrame` | `frameId` | 查询当前战斗中小于等于该 `frameId` 的已分析帧。 |
| `prepareCalibration` | 无 | 进入待校准。 |
| `startCalibration` | 无 | 开始校准。 |
| `cancelCalibration` | 无 | 请求取消正在进行的校准。 |
| `useProfile` | `filename` | 切换校准配置。 |
| `renameProfile` | `old`, `newBase` | 重命名校准配置。 |
| `deleteProfile` | `filename` | 删除校准配置；删除活动配置会回到空闲。 |
| `setDisplayMode` | `displayMode` | 设置显示模式，值为 `0_to_n-1`、`0_to_n` 或 `1_to_n`。 |
| `adjustTimer` | `frames` | 按帧调整计时器，可为负数。 |
| `resetTimer` | 无 | 重置计时器，并清空当前战斗历史帧。 |
| `undoResetTimer` | 无 | 撤销上一次重置；不清空历史。 |
| `toggleLapTimer` | 无 | 开关 lap 计时；不清空历史。 |
| `setOverlayScale` | `scale` | 设置悬浮窗缩放倍率，`1.0` 表示 100%。 |
| `saveOverlayPlacement` | `x`, `y` | 保存悬浮窗屏幕位置。 |
| `exit` | 无 | 退出尺子。 |

示例：

```json
{"type":"getSnapshot","requestId":"snap-1"}
{"type":"getFrame","requestId":"frame-4241","frameId":4241}
{"type":"adjustTimer","requestId":"minus-30","frames":-30}
{"type":"setDisplayMode","requestId":"mode","displayMode":"1_to_n"}
```

## 历史帧查询

`getFrame` 返回的是已分析状态记录，不返回原始像素数据。历史帧的编号统一使用截图管线的 `Frame.id`。

如果请求的帧被 `SkipToLatest` 跳过，或因为 PC 光标遮挡没有写入分析历史，服务会回退到 `<= requestedFrameId` 的上一条已分析记录：

```json
{
  "type": "frame",
  "requestId": "frame-4241",
  "requestedFrameId": 4241,
  "actualFrameId": 4240,
  "fellBack": true,
  "fallbackReason": "frame_skipped",
  "frame": {
    "frameId": 4240,
    "sampleIndex": 310,
    "droppedSincePrevious": 1,
    "isRunning": true,
    "currentFrame": 14,
    "totalFramesInCycle": 30,
    "totalElapsedFrames": 74,
    "activeProfile": "正常回费",
    "rawPixelWidth": 122,
    "costIsNegative": false,
    "battleState": "battle",
    "captureWidth": 1280,
    "captureHeight": 720,
    "captureFormat": "rgba",
    "captureTimestampNs": 1234560000,
    "captureDurationUs": 1500
  }
}
```

`fallbackReason` 目前可能是：

- `frame_skipped`: 请求帧位于保留历史范围内，但该帧没有已分析记录。
- `requested_after_latest`: 请求帧晚于当前最新已分析记录。

如果当前战斗里没有任何 `<= requestedFrameId` 的已分析记录，则返回：

```json
{
  "type": "error",
  "requestId": "frame-old",
  "code": "frame_not_retained",
  "message": "no analyzed frame retained at or before frameId 123"
}
```

历史会在当前战斗边界清空：手动 `resetTimer`、自动识别到 `BattleBegin` 重置、新建 / 切换 / 删除活动校准、进入校准，以及校准失败回到待校准时都会清空。`adjustTimer`、`undoResetTimer`、`toggleLapTimer` 不会清空。

## HTTP 快照

HTTP 只提供当前快照，不处理写命令：

```bash
curl http://127.0.0.1:2606/
```

响应带 `Access-Control-Allow-Origin: *`，所以网页里用 `fetch` 跨域取用也没问题：

```javascript
const data = await fetch("http://127.0.0.1:2606/").then(r => r.json());
console.log(data.frameId, data.currentFrame, data.totalFramesInCycle);
```

## 最小 WebSocket 例子

```python
import asyncio
import json
import websockets

async def main():
    async for ws in websockets.connect("ws://127.0.0.1:2606/"):
        try:
            await ws.send(json.dumps({
                "type": "getSnapshot",
                "requestId": "hello",
            }))
            async for message in ws:
                print(json.loads(message))
        except websockets.ConnectionClosed:
            continue

asyncio.run(main())
```
