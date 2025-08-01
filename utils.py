import datetime
import logging
import os
import sys
from typing import Optional, Tuple, Dict

from PIL import Image, ImageDraw

from logger_setup import DEBUG_IMAGE_MODE, IMG_DUMP_DIR

logger = logging.getLogger(__name__)


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


def dump_image_with_roi(image: Image.Image, roi: tuple, prefix: str, info_text: str = ""):
    """
    如果启用了调试模式，则将带有ROI框的图像转储到日志目录。

    Args:
        image (Image.Image): 要保存的原始图像。
        roi (tuple): (x1, x2, y) 格式的ROI。
        prefix (str): 文件名前缀，例如 "calib_frame_123"。
        info_text (str): 要绘制在图像上的额外信息文本。
    """
    if not DEBUG_IMAGE_MODE or not IMG_DUMP_DIR:
        return

    try:
        # 创建图像副本以进行绘制，避免修改原始图像
        img_copy = image.copy().convert("RGB")
        draw = ImageDraw.Draw(img_copy)

        x1, x2, y = roi
        # 绘制红色的ROI线
        draw.line([(x1, y), (x2, y)], fill="red", width=2)
        # 在ROI两端绘制标记
        draw.line([(x1, y - 5), (x1, y + 5)], fill="yellow", width=2)
        draw.line([(x2, y - 5), (x2, y + 5)], fill="yellow", width=2)

        # 在图像左上角绘制信息文本
        if info_text:
            draw.text((10, 10), info_text, fill="lime")

        # 生成唯一的文件名
        timestamp = datetime.datetime.now().strftime('%H%M%S_%f')[:-3]
        filename = f"{prefix}_{timestamp}.jpg"
        filepath = os.path.join(IMG_DUMP_DIR, filename)

        # 保存图像
        img_copy.save(filepath, quality=90)
        logger.debug(f"图像已转储: {filepath}")

    except Exception as e:
        logger.error(f"转储调试图像时出错: {e}")


def find_cost_bar_roi(screen_width: int, screen_height: int) -> tuple[int, int, int]:
    """
    根据屏幕分辨率计算明日方舟费用条的位置。
    """
    # 参考分辨率和比例
    REF_WIDTH, REF_HEIGHT = 1920.0, 1080.0
    REF_ASPECT_RATIO = REF_WIDTH / REF_HEIGHT
    # 在参考分辨率下，费用条距离屏幕边缘的偏移量
    X1_OFFSET_FROM_RIGHT_REF = REF_WIDTH - 1739
    X2_OFFSET_FROM_RIGHT_REF = REF_WIDTH - 1919
    Y1_OFFSET_FROM_BOTTOM_REF = REF_HEIGHT - 810
    Y2_OFFSET_FROM_BOTTOM_REF = REF_HEIGHT - 817

    current_aspect_ratio = screen_width / screen_height

    # 根据当前屏幕是更宽还是更高来决定缩放基准
    if current_aspect_ratio >= REF_ASPECT_RATIO:
        # 屏幕比16:9更宽（例如带鱼屏），以高度为基准缩放
        scale = screen_height / REF_HEIGHT
    else:
        # 屏幕比16:9更高（例如平板），以宽度为基准缩放
        scale = screen_width / REF_WIDTH

    # 计算缩放后的坐标
    x1 = screen_width - X1_OFFSET_FROM_RIGHT_REF * scale
    x2 = screen_width - X2_OFFSET_FROM_RIGHT_REF * scale
    y1 = screen_height - Y1_OFFSET_FROM_BOTTOM_REF * scale
    y2 = screen_height - Y2_OFFSET_FROM_BOTTOM_REF * scale

    # 四舍五入取整
    x1_int, x2_int = round(x1), round(x2)
    y_mid_int = round((y1 + y2) / 2)

    logger.debug(f"为 {screen_width}x{screen_height} 计算出ROI: x1={x1_int}, x2={x2_int}, y={y_mid_int}")
    return (x1_int, x2_int, y_mid_int)


def _get_raw_filled_pixel_width(
        frame: Image.Image,
        roi: tuple[int, int, int],
        dump_prefix: Optional[str] = None
) -> Optional[int]:
    """
    从费用条ROI中提取填充部分的像素宽度。
    此版本经过强化，会进行严格的颜色和透明度校验，以确保ROI确实是费用条。
    """
    # 1. 定义严格的校验常量
    WHITE_THRESHOLD = 250
    GRAY_TOLERANCE = 10
    ALPHA_OPAQUE = 255

    # 2. 内联辅助函数，用于检查灰度
    def is_pixel_grayscale(r, g, b):
        return abs(r - g) <= GRAY_TOLERANCE and abs(g - b) <= GRAY_TOLERANCE

    # 3. ROI基础检查
    x1, x2, y = roi
    total_width = x2 - x1
    if total_width <= 0:
        return None

    # 4. 确保图像为RGBA模式
    if frame.mode != 'RGBA':
        frame = frame.convert('RGBA')

    # 5. 【核心校验】对费用条末端像素进行严格检查
    try:
        r_end, g_end, b_end, a_end = frame.getpixel((x2 - 1, y))
    except IndexError:
        logger.warning(f"ROI超出图像边界: roi={roi}, image_size={frame.size}")
        return None  # ROI超出图像边界

    if a_end != ALPHA_OPAQUE or not is_pixel_grayscale(r_end, g_end, b_end):
        logger.debug("ROI区域无效: 末端像素不是不透明的灰度色。")
        return None

    # 6. 【边界扫描】
    is_end_pixel_white = all(c > WHITE_THRESHOLD for c in (r_end, g_end, b_end))
    if is_end_pixel_white:
        filled_width = total_width
        logger.debug(f"费用条已满 (末端像素为白色)，宽度: {filled_width}")
        if dump_prefix:
            dump_image_with_roi(frame, roi, dump_prefix, f"RawWidth: {filled_width} (Full)")
        return filled_width

    filled_width = 0
    # 从右向左扫描，寻找白色与灰色的边界
    for x in range(x2 - 2, x1 - 1, -1):
        r, g, b, a = frame.getpixel((x, y))

        if a != ALPHA_OPAQUE or not is_pixel_grayscale(r, g, b):
            logger.debug(f"ROI区域在扫描时发现无效像素 (x={x})，判定为非费用条。")
            return None

        is_current_pixel_white = all(c > WHITE_THRESHOLD for c in (r, g, b))
        if is_current_pixel_white:
            filled_width = x - x1 + 1
            break

    logger.debug(f"扫描完成，检测到填充宽度: {filled_width}")
    if dump_prefix:
        dump_image_with_roi(frame, roi, dump_prefix, f"RawWidth: {filled_width}")

    return filled_width


def get_logical_frame_from_calibration(
        frame: Image.Image,
        roi: Tuple[int, int, int],
        calibration_data: Dict[str, any],
        dump_prefix: Optional[str] = None
) -> Optional[int]:
    """
    使用校准数据将当前费用条状态转换为逻辑帧。
    """
    current_pixel_width = _get_raw_filled_pixel_width(frame, roi, dump_prefix=dump_prefix)
    if current_pixel_width is None:
        logger.debug("无法获取原始像素宽度，逻辑帧判定为None。")
        return None

    pixel_map = calibration_data['pixel_map']

    # 1. 尝试直接匹配
    if str(current_pixel_width) in pixel_map:
        logical_frame = pixel_map[str(current_pixel_width)]
        logger.debug(f"原始宽度 {current_pixel_width} 直接匹配到逻辑帧 {logical_frame}")
        return logical_frame

    # 2. 如果没有直接匹配，则寻找最近的值
    closest_pixel_value = -1
    min_diff = float('inf')

    for pixel_str in pixel_map.keys():
        pixel_val = int(pixel_str)
        diff = abs(current_pixel_width - pixel_val)
        if diff < min_diff:
            min_diff = diff
            closest_pixel_value = pixel_val

    # 设置一个容差，如果差异过大，则认为是不匹配的状态
    TOLERANCE = 5
    if min_diff <= TOLERANCE:
        logical_frame = pixel_map[str(closest_pixel_value)]
        logger.debug(
            f"原始宽度 {current_pixel_width} 近似匹配到 {closest_pixel_value} (差异 {min_diff}), 逻辑帧 {logical_frame}")
        return logical_frame
    else:
        logger.warning(f"原始宽度 {current_pixel_width} 未能匹配到任何校准值 (最小差异 {min_diff} > {TOLERANCE})")
        return None