import asyncio
import websockets
import json


async def listen_to_ruler():
    uri = "ws://localhost:2606"
    while True:
        try:
            async with websockets.connect(uri) as websocket:
                print(f"成功连接到尺子API: {uri}")
                while True:
                    message = await websocket.recv()
                    data = json.loads(message)

                    # 在这里，你可以开始构建你的打轴器逻辑
                    if data["isRunning"]:
                        total_frames = data['totalElapsedFrames']
                        minutes = total_frames // (30 * 60)
                        seconds = (total_frames // 30) % 60
                        frames = total_frames % 30

                        print(
                            f"\r当前配置: {data['activeProfile']} | 运行时间: {minutes:02d}:{seconds:02d}:{frames:02d} | 帧: {data['currentFrame']}/{data['totalFramesInCycle'] - 1}  ",
                            end="")
                    else:
                        print("\r尺子未在运行或未检测到费用条...", end="")

        except (ConnectionRefusedError, websockets.exceptions.ConnectionClosed) as e:
            print(f"\n无法连接到尺子API，1秒后重试... ({e})")
            await asyncio.sleep(1)


if __name__ == "__main__":
    try:
        asyncio.run(listen_to_ruler())
    except KeyboardInterrupt:
        print("\n客户端已关闭。")