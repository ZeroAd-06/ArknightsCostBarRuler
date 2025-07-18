import ctypes
from ctypes import wintypes
from pathlib import Path
import sys
import time
from typing import Optional

from PIL import Image

if __name__ == '__main__':
    from base import BaseCaptureController
else:
    from .base import BaseCaptureController

class MuMuPlayerController(BaseCaptureController):
    """
    通过加载 MuMu 模拟器的`external_renderer_ipc.dll`来获取屏幕截图。
    此方法专为解决 MuMu 模拟器的兼容性问题。

    """

    def __init__(self, mumu_install_path: str, instance_index: int = 0):
        """
        初始化 MuMuPlayerController。

        Args:
            mumu_install_path (str): MuMu 模拟器的安装根目录。
            instance_index (int): 模拟器实例的索引，用于多开场景，默认为0。
        """
        if sys.platform != "win32":
            raise NotImplementedError("MuMuPlayerController 仅支持 Windows 平台。")

        self.install_path = Path(mumu_install_path)
        if not self.install_path.exists():
            raise FileNotFoundError(f"指定的MuMu模拟器路径不存在: {self.install_path}")

        self.instance_index = instance_index

        # DLL 和函数句柄
        self.dll: Optional[ctypes.WinDLL] = None
        self.handle: int = 0

        # 图像缓冲
        self.width: int = 0
        self.height: int = 0
        self.buffer: Optional[ctypes.Array] = None

    def _find_and_load_dll(self):
        """在MuMu安装目录中查找并加载核心DLL。"""
        # 可能的DLL路径
        dll_path_options = [
            self.install_path / "nx_main" / "sdk" / "external_renderer_ipc.dll",
            self.install_path / "shell" / "sdk" / "external_renderer_ipc.dll",
        ]

        dll_path = None
        for path in dll_path_options:
            if path.exists():
                dll_path = path
                break

        if not dll_path:
            raise FileNotFoundError("在指定的MuMu安装目录中未找到 'external_renderer_ipc.dll'。")

        print(f">>> 正在加载DLL: {dll_path}")
        self.dll = ctypes.WinDLL(str(dll_path))
        print("    DLL加载成功。")

    def _setup_function_prototypes(self):
        """定义从DLL中调用的函数的参数类型和返回类型。"""
        self.dll.nemu_connect.argtypes = [wintypes.LPCWSTR, ctypes.c_int]
        self.dll.nemu_connect.restype = ctypes.c_int

        self.dll.nemu_disconnect.argtypes = [ctypes.c_int]

        self.dll.nemu_capture_display.argtypes = [
            ctypes.c_int,  # handle
            ctypes.c_int,  # display_id
            ctypes.c_int,  # buffer_size
            ctypes.POINTER(ctypes.c_int),  # *width
            ctypes.POINTER(ctypes.c_int),  # *height
            ctypes.POINTER(ctypes.c_ubyte)  # *buffer
        ]
        self.dll.nemu_capture_display.restype = ctypes.c_int

    def connect(self):
        """加载DLL，连接到模拟器实例并初始化截图环境。"""
        self._find_and_load_dll()
        self._setup_function_prototypes()

        print(">>> 正在连接到MuMu实例...")
        self.handle = self.dll.nemu_connect(str(self.install_path), self.instance_index)
        if self.handle == 0:
            raise ConnectionError(
                f"连接MuMu失败 (handle=0)。请确认模拟器正在运行，且路径 '{self.install_path}' 和索引 '{self.instance_index}' 正确。")
        print(f"    连接成功，获得句柄: {self.handle}")

        print(">>> 正在初始化截图...")
        # 第一次调用以获取屏幕尺寸
        width_ptr = ctypes.pointer(ctypes.c_int())
        height_ptr = ctypes.pointer(ctypes.c_int())
        # 成功时返回0
        ret = self.dll.nemu_capture_display(self.handle, 0, 0, width_ptr, height_ptr, None)
        if ret != 0:
            raise RuntimeError(f"获取屏幕尺寸失败，错误码: {ret}")

        self.width = width_ptr.contents.value
        self.height = height_ptr.contents.value
        print(f"    获取到屏幕尺寸: {self.width}x{self.height}")

        # 根据尺寸创建图像缓冲区
        buffer_size = self.width * self.height * 4  # RGBA格式，4字节/像素
        self.buffer = (ctypes.c_ubyte * buffer_size)()
        print(f"    图像缓冲区已创建 (大小: {buffer_size} 字节)。")

    def capture_frame(self) -> Image.Image:
        """
        捕获一帧屏幕图像。
        """
        if not all([self.dll, self.handle, self.buffer]):
            raise ConnectionError("未连接或初始化失败。请先调用 connect()。")

        ret = self.dll.nemu_capture_display(
            self.handle,
            0,  # display_id, 默认为0
            len(self.buffer),
            ctypes.pointer(ctypes.c_int(self.width)),
            ctypes.pointer(ctypes.c_int(self.height)),
            self.buffer
        )

        if ret != 0:
            raise RuntimeError(f"截图失败，错误码: {ret}")

        return self.conv()


    def conv(self):

        # 从原始RGBA缓冲区创建Pillow图像
        image_raw = Image.frombuffer('RGBA', (self.width, self.height), self.buffer, 'raw', 'RGBA', 0, 1)
        image_flipped = image_raw.transpose(Image.FLIP_TOP_BOTTOM)

        # 转换为RGB格式，便于后续处理和保存
        return image_flipped.convert('RGB')

    def disconnect(self):
        """断开与MuMu实例的连接。"""
        if self.dll and self.handle != 0:
            print(">>> 正在断开与MuMu的连接...")
            self.dll.nemu_disconnect(self.handle)
            self.handle = 0
            print("    断开连接成功。")

    def __enter__(self):
        self.connect()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.disconnect()


if __name__ == '__main__':
    MUMU_PATH = "D:\Game\Android\YXArkNights-12.0"

    if not MUMU_PATH:
        print("错误: 请在脚本中设置 'MUMU_PATH' 变量为您的MuMu模拟器安装路径。")
    else:
        try:
            with MuMuPlayerController(mumu_install_path=MUMU_PATH) as mumu_cap:
                for i in range(600):
                    print("\n--- 开始捕获 ---")
                    start_time = time.time()

                    frame = mumu_cap.capture_frame()

                    end_time = time.time()

                    print(f"成功捕获一帧! 分辨率: {frame.size}, 耗时: {end_time - start_time:.4f} 秒")

                    save_path = "mumu_capture.jpg"
                    frame.save(save_path)
                    print(f"图像已保存至: {save_path}")

        except (NotImplementedError, FileNotFoundError, ConnectionError, RuntimeError) as e:
            print(f"\n!!! 程序运行出错: {e}")
        except KeyboardInterrupt:
            print("\n用户手动中断程序。")