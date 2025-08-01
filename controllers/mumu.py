'''
本模块的实现逻辑参考了 MaaFramework( https://github.com/MaaXYZ/MaaFramework )
因此本文件遵循LGPL-3.0协议
如果你不需要在 MuMu 模拟器12 上使用本程序，你可以删除这一文件。
'''
import ctypes
import logging
from ctypes import wintypes
from pathlib import Path
import sys
import time
from typing import Optional, Tuple

from PIL import Image

if __name__ == '__main__':
    from base import BaseCaptureController
else:
    from .base import BaseCaptureController

logger = logging.getLogger(__name__)


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
        logger.info(f"MuMuPlayerController 初始化: path='{mumu_install_path}', instance={instance_index}")
        if sys.platform != "win32":
            logger.critical("MuMuPlayerController 仅支持 Windows 平台。")
            raise NotImplementedError("MuMuPlayerController 仅支持 Windows 平台。")

        self.install_path = Path(mumu_install_path)
        if not self.install_path.exists():
            logger.error(f"指定的MuMu模拟器路径不存在: {self.install_path}")
            raise FileNotFoundError(f"指定的MuMu模拟器路径不存在: {self.install_path}")

        self.instance_index = instance_index

        # DLL 和函数句柄
        self.dll: Optional[ctypes.WinDLL] = None
        self.handle: int = 0

        # 图像缓冲
        self.width: int = 0
        self.height: int = 0
        self.buffer: Optional[ctypes.Array] = None

    def _find_and_load_dll(self) -> Tuple[Path, Path]:
        """
        在MuMu安装目录中智能查找并返回核心DLL的路径和正确的根目录。

        Returns:
            Tuple[Path, Path]: 一个元组，包含(找到的DLL的绝对路径, 修正后的MuMu根目录路径)。

        Raises:
            FileNotFoundError: 如果在所有可能的路径中都找不到DLL。
        """
        logger.info(f"开始在 '{self.install_path}' 及其父目录中查找DLL...")
        initial_path = self.install_path
        # 创建一个搜索路径列表，包含用户提供的路径及其父目录
        search_bases = [initial_path]
        if initial_path.parent != initial_path:  # 避免在根目录(如C:\)时重复添加
            search_bases.append(initial_path.parent)
        logger.debug(f"搜索基准路径: {search_bases}")

        # 可能的DLL相对路径
        relative_dll_paths = [
            Path("nx_main") / "sdk" / "external_renderer_ipc.dll",
            Path("shell") / "sdk" / "external_renderer_ipc.dll",
        ]
        logger.debug(f"可能的相对DLL路径: {relative_dll_paths}")

        for base in search_bases:
            for rel_path in relative_dll_paths:
                dll_candidate_path = base / rel_path
                logger.debug(f"正在尝试路径: {dll_candidate_path}")
                if dll_candidate_path.exists():
                    logger.info(f"在 '{base}' 找到了DLL: {dll_candidate_path}")
                    # 找到了！返回DLL的完整路径和它所在的正确根目录
                    return dll_candidate_path, base

        # 如果循环结束都没有找到
        logger.error("在指定的MuMu安装目录中未找到 'external_renderer_ipc.dll'")
        raise FileNotFoundError(
            "在指定的MuMu安装目录中未找到 'external_renderer_ipc.dll'。\n"
            "请确保路径正确，可以提供MuMu的根目录或其下的'shell'子目录。"
        )

    def _setup_function_prototypes(self):
        """定义从DLL中调用的函数的参数类型和返回类型。"""
        logger.debug("正在设置DLL函数原型...")
        self.dll.nemu_connect.argtypes = [wintypes.LPCWSTR, ctypes.c_int]
        self.dll.nemu_connect.restype = ctypes.c_int
        logger.debug("nemu_connect.argtypes/restype 设置完成。")

        self.dll.nemu_disconnect.argtypes = [ctypes.c_int]
        logger.debug("nemu_disconnect.argtypes 设置完成。")

        self.dll.nemu_capture_display.argtypes = [
            ctypes.c_int,  # handle
            ctypes.c_int,  # display_id
            ctypes.c_int,  # buffer_size
            ctypes.POINTER(ctypes.c_int),  # *width
            ctypes.POINTER(ctypes.c_int),  # *height
            ctypes.POINTER(ctypes.c_ubyte)  # *buffer
        ]
        self.dll.nemu_capture_display.restype = ctypes.c_int
        logger.debug("nemu_capture_display.argtypes/restype 设置完成。")

    def connect(self):
        """加载DLL，连接到模拟器实例并初始化截图环境。"""
        logger.info("开始连接到 MuMu 实例...")
        # 智能查找DLL，并获取正确的DLL路径和根目录
        dll_path, correct_root_path = self._find_and_load_dll()

        # 更新实例的安装路径为修正后的路径
        self.install_path = correct_root_path
        logger.info(f"修正后的MuMu根目录: {self.install_path}")

        logger.info(f"正在加载DLL: {dll_path}")
        self.dll = ctypes.WinDLL(str(dll_path))
        logger.info("DLL加载成功。")

        self._setup_function_prototypes()

        logger.info("正在连接到MuMu实例...")
        # 使用修正后的根目录进行连接
        self.handle = self.dll.nemu_connect(str(self.install_path), self.instance_index)
        if self.handle == 0:
            logger.error(f"连接MuMu失败 (handle=0)。路径='{self.install_path}', 索引={self.instance_index}")
            raise ConnectionError(
                f"连接MuMu失败 (handle=0)。请确认模拟器正在运行，且路径 '{self.install_path}' 和索引 '{self.instance_index}' 正确。")
        logger.info(f"连接成功，获得句柄: {self.handle}")

        logger.info("正在初始化截图...")
        width_ptr = ctypes.pointer(ctypes.c_int())
        height_ptr = ctypes.pointer(ctypes.c_int())
        logger.debug("调用 nemu_capture_display 以获取屏幕尺寸。")
        ret = self.dll.nemu_capture_display(self.handle, 0, 0, width_ptr, height_ptr, None)
        if ret != 0:
            logger.error(f"获取屏幕尺寸失败，错误码: {ret}")
            raise RuntimeError(f"获取屏幕尺寸失败，错误码: {ret}")

        self.width = width_ptr.contents.value
        self.height = height_ptr.contents.value
        logger.info(f"获取到屏幕尺寸: {self.width}x{self.height}")

        buffer_size = self.width * self.height * 4
        self.buffer = (ctypes.c_ubyte * buffer_size)()
        logger.info(f"图像缓冲区已创建 (大小: {buffer_size} 字节)。")
        return self

    def capture_frame(self) -> Image.Image:
        """
        捕获一帧屏幕图像。
        """
        if not all([self.dll, self.handle, self.buffer]):
            logger.error("捕获帧失败：未连接或初始化失败。")
            raise ConnectionError("未连接或初始化失败。请先调用 connect()。")

        logger.debug("调用 nemu_capture_display 进行截图...")
        ret = self.dll.nemu_capture_display(
            self.handle,
            0,
            len(self.buffer),
            ctypes.pointer(ctypes.c_int(self.width)),
            ctypes.pointer(ctypes.c_int(self.height)),
            self.buffer
        )

        if ret != 0:
            logger.error(f"截图失败，错误码: {ret}")
            raise RuntimeError(f"截图失败，错误码: {ret}")

        logger.debug("截图数据已写入缓冲区，准备转换格式。")
        return self.conv()

    def conv(self) -> Image.Image:
        """将原始缓冲区数据转换为 PIL Image 对象。"""
        logger.debug(f"从缓冲区创建RGBA图像 ({self.width}x{self.height})...")
        image_raw = Image.frombuffer('RGBA', (self.width, self.height), self.buffer, 'raw', 'RGBA', 0, 1)
        logger.debug("垂直翻转图像...")
        image_flipped = image_raw.transpose(Image.FLIP_TOP_BOTTOM)
        logger.debug("将图像转换为RGB模式...")
        return image_flipped.convert('RGB')

    def disconnect(self):
        """断开与MuMu实例的连接。"""
        if self.dll and self.handle != 0:
            logger.info("正在断开与MuMu的连接...")
            self.dll.nemu_disconnect(self.handle)
            self.handle = 0
            logger.info("断开连接成功。")
        else:
            logger.debug("无需断开连接 (dll或handle为空)。")

    def __enter__(self):
        self.connect()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.disconnect()


if __name__ == '__main__':
    from logger_setup import setup_logging
    setup_logging(debug_image_mode=True)
    main_logger = logging.getLogger(__name__)

    MUMU_PATH = r"D:\Game\Android\YXArkNights-12.0\shell"
    main_logger.info(f"测试 MuMuPlayerController，路径: {MUMU_PATH}")

    try:
        with MuMuPlayerController(mumu_install_path=MUMU_PATH) as mumu_cap:
            main_logger.info("--- 开始捕获 ---")
            start_time = time.time()
            frame = mumu_cap.capture_frame()
            end_time = time.time()

            main_logger.info(f"成功捕获一帧! 分辨率: {frame.size}, 耗时: {end_time - start_time:.4f} 秒")
            save_path = "mumu_capture.jpg"
            frame.save(save_path)
            main_logger.info(f"图像已保存至: {save_path}")

    except (NotImplementedError, FileNotFoundError, ConnectionError, RuntimeError) as e:
        main_logger.exception(f"程序运行出错: {e}")
    except KeyboardInterrupt:
        main_logger.info("用户手动中断程序。")
