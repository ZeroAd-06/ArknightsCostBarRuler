import ctypes
import logging
import time
from ctypes import wintypes
from pathlib import Path
from PIL import Image

try:
    from .base import BaseCaptureController
except ImportError:
    from base import BaseCaptureController

logger = logging.getLogger(__name__)

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32

class WindowsWindowController(BaseCaptureController):
    """通过 Win32 API 对指定窗口进行截图的控制器。"""

    PW_CLIENTONLY = 0x00000001
    PW_RENDERFULLCONTENT = 0x00000002
    SRCCOPY = 0x00CC0020

    def __init__(self, hwnd: int = None, window_title: str = None, class_name: str = None):
        self.hwnd = None
        if hwnd is not None:
            self.hwnd = wintypes.HWND(hwnd)
        self.window_title = window_title
        self.class_name = class_name
        self.width = 0
        self.height = 0
        self.client_left = 0
        self.client_top = 0
        self.hdc_window = None
        self.hdc_mem = None
        self.bmp = None

    def _find_matching_hwnd(self):
        if not self.window_title:
            return None

        GetTopWindow = user32.GetTopWindow
        GetWindow = user32.GetWindow
        GetWindowTextLengthW = user32.GetWindowTextLengthW
        GetWindowTextW = user32.GetWindowTextW
        GetClassNameW = user32.GetClassNameW
        IsWindowVisible = user32.IsWindowVisible

        hwnd = GetTopWindow(None)
        GW_HWNDNEXT = 2
        while hwnd:
            if IsWindowVisible(hwnd):
                length = GetWindowTextLengthW(hwnd)
                title = ""
                if length > 0:
                    buffer = ctypes.create_unicode_buffer(length + 1)
                    GetWindowTextW(hwnd, buffer, length + 1)
                    title = buffer.value.strip()
                class_name_buffer = ctypes.create_unicode_buffer(256)
                GetClassNameW(hwnd, class_name_buffer, 256)
                class_name = class_name_buffer.value.strip()

                if self.window_title and self.window_title in title:
                    if self.class_name:
                        if self.class_name.lower() in class_name.lower():
                            return hwnd
                    else:
                        return hwnd
                elif self.class_name and self.class_name.lower() in class_name.lower():
                    return hwnd
            hwnd = GetWindow(hwnd, GW_HWNDNEXT)
        return None

    def connect(self):
        if self.hwnd is not None:
            if not user32.IsWindow(self.hwnd):
                self.hwnd = None

        if self.hwnd is None:
            found = self._find_matching_hwnd()
            if not found:
                raise ConnectionError("未找到匹配的窗口句柄")
            self.hwnd = wintypes.HWND(found)

        logger.info(f"开始连接到窗口 (hwnd=0x{self.hwnd.value:08X})")
        if not user32.IsWindow(self.hwnd):
            raise ConnectionError(f"窗口句柄无效: {self.hwnd.value}")

        rect = wintypes.RECT()
        if not user32.GetClientRect(self.hwnd, ctypes.byref(rect)):
            raise ConnectionError(f"无法获取窗口客户区: hwnd=0x{self.hwnd.value:08X}")

        self.width = rect.right - rect.left
        self.height = rect.bottom - rect.top
        if self.width <= 0 or self.height <= 0:
            raise ConnectionError(f"窗口客户区大小无效: {self.width}x{self.height}")

        pt = wintypes.POINT(0, 0)
        if not user32.ClientToScreen(self.hwnd, ctypes.byref(pt)):
            raise ConnectionError(f"无法将客户区坐标转换为屏幕坐标: hwnd=0x{self.hwnd.value:08X}")

        self.client_left = pt.x
        self.client_top = pt.y

        self.hdc_window = user32.GetWindowDC(self.hwnd)
        if not self.hdc_window:
            raise ConnectionError("获取窗口设备上下文失败")

        self.hdc_mem = gdi32.CreateCompatibleDC(self.hdc_window)
        if not self.hdc_mem:
            user32.ReleaseDC(self.hwnd, self.hdc_window)
            raise ConnectionError("创建兼容设备上下文失败")

        self.bmp = gdi32.CreateCompatibleBitmap(self.hdc_window, self.width, self.height)
        if not self.bmp:
            gdi32.DeleteDC(self.hdc_mem)
            user32.ReleaseDC(self.hwnd, self.hdc_window)
            raise ConnectionError("创建兼容位图失败")

        gdi32.SelectObject(self.hdc_mem, self.bmp)
        logger.info(f"连接成功，窗口客户区分辨率: {self.width}x{self.height}")
        return self

    def capture_frame(self) -> Image.Image:
        if not self.hdc_window or not self.hdc_mem or not self.bmp:
            raise ConnectionError("尚未初始化截图资源，请先 connect()")

        # 检查窗口是否被最小化
        if user32.IsIconic(self.hwnd):
            # 如果最小化了，尝试恢复但不激活，截图完后再最小化（可选方案，但通常不建议自动操作用户窗口）
            # 这里我们选择抛出更明确的错误，或者尝试一种轻量级恢复
            logger.warning(f"窗口 0x{self.hwnd.value:08X} 处于最小化状态，截图可能失效。")
            # 某些应用在最小化时仍能通过 PrintWindow 截图，所以我们继续尝试

        capture_ok = False
        
        # 1. 优先尝试 PrintWindow (支持后台/遮挡截图)
        # PW_RENDERFULLCONTENT (0x02) 在 Win8.1+ 上对硬件加速窗口更有效
        # 我们先尝试带 PW_CLIENTONLY 的，再尝试不带的
        for flags in [self.PW_CLIENTONLY | self.PW_RENDERFULLCONTENT, self.PW_RENDERFULLCONTENT]:
            if user32.PrintWindow(self.hwnd, self.hdc_mem, flags):
                # 简单检查是否截到了全黑（硬件加速窗口常见问题）
                # 这里我们无法直接检查位图内容，但 PrintWindow 返回 True 通常意味着系统尝试了渲染
                capture_ok = True
                break

        # 2. 如果 PrintWindow 失败，尝试直接从窗口 DC BitBlt (部分非硬件加速窗口支持后台)
        if not capture_ok:
            if gdi32.BitBlt(self.hdc_mem, 0, 0, self.width, self.height, self.hdc_window, 0, 0, self.SRCCOPY):
                capture_ok = True

        # 3. 最后回退到 BitBlt 从屏幕 DC (会被遮挡)
        if not capture_ok:
            screen_dc = user32.GetDC(None)
            if gdi32.BitBlt(self.hdc_mem, 0, 0, self.width, self.height, screen_dc, self.client_left, self.client_top, self.SRCCOPY):
                capture_ok = True
            user32.ReleaseDC(None, screen_dc)

        if not capture_ok:
            raise RuntimeError("窗口截图所有方案均失败")

        class BITMAPINFOHEADER(ctypes.Structure):
            _fields_ = [
                ("biSize", wintypes.DWORD),
                ("biWidth", wintypes.LONG),
                ("biHeight", wintypes.LONG),
                ("biPlanes", wintypes.WORD),
                ("biBitCount", wintypes.WORD),
                ("biCompression", wintypes.DWORD),
                ("biSizeImage", wintypes.DWORD),
                ("biXPelsPerMeter", wintypes.LONG),
                ("biYPelsPerMeter", wintypes.LONG),
                ("biClrUsed", wintypes.DWORD),
                ("biClrImportant", wintypes.DWORD),
            ]

        class BITMAPINFO(ctypes.Structure):
            _fields_ = [
                ("bmiHeader", BITMAPINFOHEADER),
                ("bmiColors", wintypes.DWORD * 3)
            ]

        bmi = BITMAPINFO()
        bmi.bmiHeader.biSize = ctypes.sizeof(BITMAPINFOHEADER)
        bmi.bmiHeader.biWidth = self.width
        bmi.bmiHeader.biHeight = -self.height
        bmi.bmiHeader.biPlanes = 1
        bmi.bmiHeader.biBitCount = 24
        bmi.bmiHeader.biCompression = 0
        bmi.bmiHeader.biSizeImage = 0

        buffer_size = self.width * self.height * 3
        buffer = ctypes.create_string_buffer(buffer_size)

        bits = gdi32.GetDIBits(self.hdc_mem, self.bmp, 0, self.height, buffer, ctypes.byref(bmi), 0)
        if bits == 0:
            raise RuntimeError("从位图获取像素数据失败")

        image = Image.frombuffer('RGB', (self.width, self.height), buffer, 'raw', 'BGR', 0, 1)

        return image

    def disconnect(self):
        if self.bmp:
            gdi32.DeleteObject(self.bmp)
            self.bmp = None
        if self.hdc_mem:
            gdi32.DeleteDC(self.hdc_mem)
            self.hdc_mem = None
        if self.hdc_window:
            user32.ReleaseDC(self.hwnd, self.hdc_window)
            self.hdc_window = None

    def __enter__(self):
        return self.connect()

    def __exit__(self, exc_type, exc_val, exc_tb):
        self.disconnect()
