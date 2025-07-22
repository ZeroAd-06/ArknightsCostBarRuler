import os
import sys
from typing import Optional, Tuple, Dict

from PIL import Image


def resource_path(relative_path: str) -> str:
    """
    获取资源的绝对路径，无论是从源码运行还是从打包后的exe运行。
    """
    try:
        # PyInstaller 创建一个临时文件夹，并把路径存储在 _MEIPASS 中
        base_path = sys._MEIPASS
    except Exception:
        # 如果没有 _MEIPASS 属性，说明是直接从源码运行的
        base_path = os.path.abspath(".")

    return os.path.join(base_path, relative_path)


def find_cost_bar_roi(screen_width: int, screen_height: int) -> tuple[int, int, int]:
    """
    根据屏幕分辨率计算明日方舟费用条的位置。
    """
    REF_WIDTH, REF_HEIGHT = 1920.0, 1080.0
    REF_ASPECT_RATIO = REF_WIDTH / REF_HEIGHT
    X1_OFFSET_FROM_RIGHT_REF = REF_WIDTH - 1739
    X2_OFFSET_FROM_RIGHT_REF = REF_WIDTH - 1919
    Y1_OFFSET_FROM_BOTTOM_REF = REF_HEIGHT - 810
    Y2_OFFSET_FROM_BOTTOM_REF = REF_HEIGHT - 817
    current_aspect_ratio = screen_width / screen_height
    if current_aspect_ratio >= REF_ASPECT_RATIO:
        scale = screen_height / REF_HEIGHT
    else:
        scale = screen_width / REF_WIDTH
    x1 = screen_width - X1_OFFSET_FROM_RIGHT_REF * scale
    x2 = screen_width - X2_OFFSET_FROM_RIGHT_REF * scale
    y1 = screen_height - Y1_OFFSET_FROM_BOTTOM_REF * scale
    y2 = screen_height - Y2_OFFSET_FROM_BOTTOM_REF * scale
    x1_int, x2_int = round(x1), round(x2)
    y_mid_int = round((y1 + y2) / 2)
    return (x1_int, x2_int, y_mid_int)


def _get_raw_filled_pixel_width(frame: Image.Image, roi: tuple[int, int, int]) -> Optional[int]:
    """
    从费用条ROI中提取填充部分的像素宽度。
    """
    x1, x2, y = roi
    total_width = x2 - x1

    if total_width <= 0:
        return None

    # 增加颜色判断的容差，使其更稳健
    WHITE_THRESHOLD = 230
    UNFILLED_GRAY_THRESHOLD = 90

    try:
        last_pixel_color = frame.getpixel((x2 - 1, y))
    except IndexError:
        # 如果ROI超出图像边界，则视为无效
        return None

    if all(c > UNFILLED_GRAY_THRESHOLD for c in last_pixel_color):
        if all(c > WHITE_THRESHOLD for c in last_pixel_color):
            return total_width
        return None

    filled_width = 0
    for x in range(x2 - 2, x1 - 1, -1):
        pixel = frame.getpixel((x, y))
        if all(c > WHITE_THRESHOLD for c in pixel):
            filled_width = x - x1 + 1
            break

    return filled_width


def get_logical_frame_from_calibration(
        frame: Image.Image,
        roi: Tuple[int, int, int],
        calibration_data: Dict[str, any]
) -> Optional[int]:
    """
    使用校准数据将当前费用条状态转换为逻辑帧。
    """
    current_pixel_width = _get_raw_filled_pixel_width(frame, roi)
    if current_pixel_width is None:
        return None
    pixel_map = calibration_data['pixel_map']
    closest_pixel_value = -1
    min_diff = float('inf')
    for pixel_str, frame_idx in pixel_map.items():
        pixel_val = int(pixel_str)
        diff = abs(current_pixel_width - pixel_val)
        if diff < min_diff:
            min_diff = diff
            closest_pixel_value = pixel_val
    if min_diff > 5:
        return None
    return pixel_map.get(str(closest_pixel_value))  # 使用 .get() 更安全