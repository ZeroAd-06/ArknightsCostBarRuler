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
from typing import Optional, Tuple, List

from PIL import Image

# 兼容独立运行和作为模块导入
try:
    from .base import BaseCaptureController
except ImportError:
    from base import BaseCaptureController

logger = logging.getLogger(__name__)


class MuMuPlayerController(BaseCaptureController):
    """
    通过加载 MuMu 模拟器的`external_renderer_ipc.dll`来获取屏幕截图。
    """

    def __init__(self, mumu_install_path: str, instance_index: int, package_name_list: List[str]):
        """
        初始化 MuMuPlayerController。

        Args:
            mumu_install_path (str): MuMu 模拟器的安装根目录。
            instance_index (int): 模拟器实例的索引，用于多开场景。
            package_name_list (List[str]): 尝试检测的目标应用包名列表。
        """
        logger.info(f"MuMuPlayerController 初始化: path='{mumu_install_path}', instance={instance_index}")
        if sys.platform != "win32":
            raise NotImplementedError("MuMuPlayerController 仅支持 Windows 平台。")

        self.install_path = Path(mumu_install_path)
        if not self.install_path.exists():
            raise FileNotFoundError(f"指定的MuMu模拟器路径不存在: {self.install_path}")

        self.instance_index = instance_index
        self.package_name_list = package_name_list

        self.dll: Optional[ctypes.WinDLL] = None
        self.handle: int = 0
        self.display_id: int = -1 # 初始化为-1，表示未找到

        self.width: int = 0
        self.height: int = 0
        self.buffer: Optional[ctypes.Array] = None

    def _find_and_load_dll(self) -> Tuple[Path, Path]:
        """在MuMu安装目录中智能查找并返回核心DLL的路径和正确的根目录。"""
        logger.info(f"开始在 '{self.install_path}' 及其父目录中查找DLL...")
        initial_path = self.install_path
        search_bases = [initial_path]
        if initial_path.parent != initial_path:
            search_bases.append(initial_path.parent)

        relative_dll_paths = [
            Path("nx_main") / "sdk" / "external_renderer_ipc.dll",
            Path("shell") / "sdk" / "external_renderer_ipc.dll",
        ]

        for base in search_bases:
            for rel_path in relative_dll_paths:
                dll_candidate_path = base / rel_path
                if dll_candidate_path.exists():
                    logger.info(f"在 '{base}' 找到了DLL: {dll_candidate_path}")
                    return dll_candidate_path, base

        raise FileNotFoundError(
            "在指定的MuMu安装目录中未找到 'external_renderer_ipc.dll'。\n"
            "请确保路径正确，可以提供MuMu的根目录或其下的'shell'子目录。"
        )

    def _setup_function_prototypes(self):
        """定义从DLL中调用的函数的参数类型和返回类型。"""
        logger.debug("正在设置DLL函数原型...")
        self.dll.nemu_connect.argtypes = [wintypes.LPCWSTR, ctypes.c_int]
        self.dll.nemu_connect.restype = ctypes.c_int

        self.dll.nemu_disconnect.argtypes = [ctypes.c_int]

        self.dll.nemu_capture_display.argtypes = [
            ctypes.c_int, ctypes.c_int, ctypes.c_int,
            ctypes.POINTER(ctypes.c_int), ctypes.POINTER(ctypes.c_int),
            ctypes.POINTER(ctypes.c_ubyte)
        ]
        self.dll.nemu_capture_display.restype = ctypes.c_int

        self.dll.nemu_get_display_id.argtypes = [ctypes.c_int, wintypes.LPCSTR, ctypes.c_int]
        self.dll.nemu_get_display_id.restype = ctypes.c_int
        logger.debug("DLL函数原型设置完成。")

    def connect(self):
        """加载DLL，连接到模拟器实例并初始化截图环境。"""
        logger.info("开始连接到 MuMu 实例...")
        dll_path, correct_root_path = self._find_and_load_dll()
        self.install_path = correct_root_path
        logger.info(f"修正后的MuMu根目录: {self.install_path}")

        logger.info(f"正在加载DLL: {dll_path}")
        self.dll = ctypes.WinDLL(str(dll_path))
        logger.info("DLL加载成功。")

        self._setup_function_prototypes()

        logger.info("正在连接到MuMu实例...")
        self.handle = self.dll.nemu_connect(str(self.install_path), self.instance_index)
        if self.handle == 0:
            raise ConnectionError(f"连接MuMu失败 (handle=0)。请检查实例索引({self.instance_index})是否正确。")
        logger.info(f"连接成功，获得句柄: {self.handle}")

        # --- 遍历包名列表以获取display_id ---
        logger.info(f"正在为包列表查询显示设备ID...")
        for pkg_name in self.package_name_list:
            logger.debug(f"尝试包名: '{pkg_name}'...")
            pkg_bytes = pkg_name.encode('utf-8')
            current_display_id = self.dll.nemu_get_display_id(self.handle, pkg_bytes, 0)
            if current_display_id >= 0:
                self.display_id = current_display_id
                logger.info(f"为包 '{pkg_name}' 成功获取到显示设备ID: {self.display_id}")
                break # 找到后即退出循环
            else:
                logger.debug(f"包 '{pkg_name}' 未找到或未运行 (错误码: {current_display_id})。")

        if self.display_id < 0:
            logger.warning(f"未能从任何已知包名中找到显示设备ID。将回退到主显示设备(0)。")
            self.display_id = 0

        # ------------------------------------

        logger.info("正在初始化截图...")
        width_ptr = ctypes.pointer(ctypes.c_int())
        height_ptr = ctypes.pointer(ctypes.c_int())

        # 使用获取到的 display_id 来查询屏幕尺寸
        ret = self.dll.nemu_capture_display(self.handle, self.display_id, 0, width_ptr, height_ptr, None)
        if ret != 0:
            raise RuntimeError(f"获取屏幕尺寸失败，错误码: {ret}")

        self.width = width_ptr.contents.value
        self.height = height_ptr.contents.value
        logger.info(f"获取到屏幕尺寸: {self.width}x{self.height}")

        buffer_size = self.width * self.height * 4
        self.buffer = (ctypes.c_ubyte * buffer_size)()
        logger.info(f"图像缓冲区已创建 (大小: {buffer_size} 字节)。")
        return self

    def capture_frame(self) -> Image.Image:
        """捕获一帧屏幕图像。"""
        if not all([self.dll, self.handle, self.buffer]):
            raise ConnectionError("未连接或初始化失败。请先调用 connect()。")

        ret = self.dll.nemu_capture_display(
            self.handle,
            self.display_id, # 使用正确的显示设备ID
            len(self.buffer),
            ctypes.pointer(ctypes.c_int(self.width)),
            ctypes.pointer(ctypes.c_int(self.height)),
            self.buffer
        )

        if ret != 0:
            raise RuntimeError(f"截图失败，错误码: {ret}")

        return self.conv()

    def conv(self) -> Image.Image:
        """将原始缓冲区数据转换为 PIL Image 对象。"""
        image_raw = Image.frombuffer('RGBA', (self.width, self.height), self.buffer, 'raw', 'RGBA', 0, 1)
        image_flipped = image_raw.transpose(Image.FLIP_TOP_BOTTOM)
        return image_flipped.convert('RGB')

    def disconnect(self):
        """断开与MuMu实例的连接。"""
        if self.dll and self.handle != 0:
            logger.info("正在断开与MuMu的连接...")
            self.dll.nemu_disconnect(self.handle)
            self.handle = 0
            logger.info("断开连接成功。")

    def __enter__(self):
        return self.connect()

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.disconnect()

if __name__ == '__main__':
    logging.basicConfig(level=logging.DEBUG, format='%(asctime)s - %(name)s - %(levelname)s - %(message)s')
    main_logger = logging.getLogger(__name__)

    MUMU_PATH = r"D:\Game\Android\YXArkNights-12.0\shell"
    INSTANCE = 0
    PACKAGE_LIST = ["com.hypergryph.arknights", "com.hypergryph.arknights.bilibili"]

    main_logger.info(f"测试 MuMuPlayerController, 路径: {MUMU_PATH}, 实例: {INSTANCE}")

    try:
        with MuMuPlayerController(mumu_install_path=MUMU_PATH, instance_index=INSTANCE, package_name_list=PACKAGE_LIST) as mumu_cap:
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

