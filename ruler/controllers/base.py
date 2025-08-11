from abc import ABC, abstractmethod
from PIL import Image

class BaseCaptureController(ABC):
    """
    所有截图控制器的抽象基类。
    """

    @abstractmethod
    def connect(self):
        """建立与目标的连接并初始化。"""
        return self

    @abstractmethod
    def disconnect(self):
        """断开连接并清理所有资源。"""
        pass

    @abstractmethod
    def capture_frame(self) -> Image.Image:
        """捕获一帧屏幕图像。"""
        pass

    def __enter__(self):
        self.connect()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.disconnect()