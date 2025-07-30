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
    此版本经过强化，会进行严格的颜色和透明度校验，以确保ROI确实是费用条。
    """
    # 1. 定义严格的校验常量
    # 白色阈值，250意味着R,G,B三个通道的值都必须大于250
    WHITE_THRESHOLD = 250
    # 灰度容差，允许R,G,B之间有微小的差异（例如由JPG压缩或渲染引起）
    GRAY_TOLERANCE = 10
    # Alpha通道必须为不透明
    ALPHA_OPAQUE = 255

    # 2. 内联辅助函数，用于检查灰度
    def is_pixel_grayscale(r, g, b):
        return abs(r - g) <= GRAY_TOLERANCE and abs(g - b) <= GRAY_TOLERANCE

    # 3. ROI基础检查
    x1, x2, y = roi
    total_width = x2 - x1
    if total_width <= 0:
        return None

    # 4. 确保图像为RGBA模式，以便进行后续判断
    if frame.mode != 'RGBA':
        # 这一步很关键，确保我们总能获取到4个通道的数据
        frame = frame.convert('RGBA')

    # 5. 【核心校验】对费用条末端像素进行严格检查
    try:
        r_end, g_end, b_end, a_end = frame.getpixel((x2 - 1, y))
    except IndexError:
        return None  # ROI超出图像边界

    # 5.1 Alpha通道必须不透明
    if a_end != ALPHA_OPAQUE:
        return None

    # 5.2 必须是灰度色（白色也是灰度色）
    if not is_pixel_grayscale(r_end, g_end, b_end):
        return None

    # 6. 【边界扫描】
    # 检查末端像素是否为“严格的白色”，如果是，则费用条已满
    is_end_pixel_white = all(c > WHITE_THRESHOLD for c in (r_end, g_end, b_end))
    if is_end_pixel_white:
        return total_width

    # 从右向左扫描，寻找白色与灰色的边界
    filled_width = 0
    for x in range(x2 - 2, x1 - 1, -1):  # 从倒数第二个像素开始
        r, g, b, a = frame.getpixel((x, y))

        # 在循环内部，对每个像素都进行严格校验
        if a != ALPHA_OPAQUE: return None  # 发现非不透明像素，说明不是费用条
        if not is_pixel_grayscale(r, g, b): return None  # 发现彩色像素，说明不是费用条

        # 判断当前像素是否为“严格的白色”
        is_current_pixel_white = all(c > WHITE_THRESHOLD for c in (r, g, b))

        if is_current_pixel_white:
            # 找到了白色填充部分的右边界
            filled_width = x - x1 + 1
            break  # 找到后立刻跳出循环

    # 如果循环正常结束（没有break），说明没有找到任何白色像素，filled_width 保持为 0
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