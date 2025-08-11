import asyncio
import threading
import json
import logging
import websockets
from config import WEBSOCKET_RECONNECT_DELAY

logger = logging.getLogger(__name__)

class WebsocketClient:
    def __init__(self, uri):
        self.uri = uri
        self.ws_queue = None

    def start(self, ws_queue):
        """启动WebSocket客户端线程"""
        self.ws_queue = ws_queue
        threading.Thread(target=lambda: asyncio.run(self._handler()), daemon=True).start()

    async def _handler(self):
        """处理WebSocket连接和消息接收的主循环"""
        while True:
            try:
                async with websockets.connect(self.uri) as websocket:
                    logger.info(f"成功连接到WebSocket服务器: {self.uri}")
                    async for message in websocket:
                        try:
                            self.ws_queue.put(json.loads(message))
                        except json.JSONDecodeError:
                            logger.warning(f"收到无法解析的JSON消息: {message}")
            except (websockets.exceptions.ConnectionClosedError, OSError) as e:
                logger.warning(f"WebSocket连接已关闭或失败: {e}。将在 {WEBSOCKET_RECONNECT_DELAY} 秒后重试...")
                await asyncio.sleep(WEBSOCKET_RECONNECT_DELAY)
            except Exception as e:
                logger.error(f"发生未知WebSocket错误: {e}")
                await asyncio.sleep(WEBSOCKET_RECONNECT_DELAY)

