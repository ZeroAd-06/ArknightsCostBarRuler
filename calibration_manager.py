import json
import time
import os  # 新增
from typing import Dict, Any, List, Tuple, Optional, Callable  # 新增 Callable
from collections import defaultdict
# from utils import _generate_progress_bar # 不再需要，由UI处理

from PIL import Image

from controllers.base import BaseCaptureController
from utils import find_cost_bar_roi, _get_raw_filled_pixel_width

CALIBRATION_FILE_TPL = "calibration_data_{w}x{h}.json"  # 模板化文件名


def get_cal_filename(screen_width: int, screen_height: int) -> str:
    """获取特定分辨率的校准文件名"""
    return CALIBRATION_FILE_TPL.format(w=screen_width, h=screen_height)


def save_calibration_data(data: Dict[str, Any], screen_width: int, screen_height: int):
    """保存校准数据到文件。"""
    filename = get_cal_filename(screen_width, screen_height)
    with open(filename, 'w', encoding='utf-8') as f:
        json.dump(data, f, indent=4)
    print(f"\n校准数据已保存到 '{filename}'")


def remove_calibration_data(screen_width: int, screen_height: int) -> bool:
    """移除特定分辨率的校准文件。"""
    filename = get_cal_filename(screen_width, screen_height)
    try:
        os.remove(filename)
        print(f"已移除校准文件: '{filename}'")
        return True
    except FileNotFoundError:
        print(f"未找到校准文件 '{filename}'，无需移除。")
        return False
    except Exception as e:
        print(f"移除校准文件时出错: {e}")
        return False


def load_calibration_data(screen_width: int, screen_height: int) -> Optional[Dict[str, Any]]:
    """从文件加载校准数据。"""
    filename = get_cal_filename(screen_width, screen_height)
    try:
        with open(filename, 'r', encoding='utf-8') as f:
            data = json.load(f)
            if 'pixel_map' in data and 'total_frames' in data and 'screen_width' in data and 'screen_height' in data:
                if data['screen_width'] == screen_width and data['screen_height'] == screen_height:
                    print(f"已加载校准数据: '{filename}'")
                    return data
                else:
                    print(f"警告: 找到校准文件 '{filename}' 但分辨率不匹配。")
            return None
    except FileNotFoundError:
        # print(f"未找到校准文件 '{filename}'。") # 正常情况，无需打印
        return None
    except json.JSONDecodeError:
        print(f"错误: 校准文件 '{filename}' 格式损坏。")
        return None


def calibrate(controller: BaseCaptureController, num_cycles: int = 5,
              progress_callback: Optional[Callable[[float], None]] = None) -> Dict[str, Any]:
    """
    引导用户进行费用条校准。
    Args:
        controller (BaseCaptureController): 截图控制器实例。
        num_cycles (int): 收集多少个费用条完整循环的数据。
        progress_callback (Callable): 用于报告进度的回调函数，接收一个0-100的浮点数。
    Returns:
        Dict[str, Any]: 校准后的数据字典。
    """
    print("\n--- 进入费用条校准模式 ---")
    print("请按照悬浮窗提示操作...")

    collected_pixel_widths: List[int] = []
    current_cost_state_raw: Optional[int] = None
    previous_cost_state_raw: Optional[int] = None
    cost_bar_resets = 0
    is_collecting_cycle = False

    start_time = time.time()

    # 获取一次尺寸信息，避免在循环内重复获取
    frame = controller.capture_frame()
    width, height = frame.size

    print("\n开始收集数据...")
    while cost_bar_resets < num_cycles:
        try:
            frame = controller.capture_frame()
            roi = find_cost_bar_roi(width, height)
            current_cost_state_raw = _get_raw_filled_pixel_width(frame, roi)

            # --- 进度计算与回调 ---
            total_bar_width = roi[1] - roi[0]
            if current_cost_state_raw is not None and total_bar_width > 0:
                current_fill_percentage = current_cost_state_raw / total_bar_width
            else:
                current_fill_percentage = 0.0

            overall_progress = (cost_bar_resets + current_fill_percentage) / num_cycles
            progress_percent = min(100.0, overall_progress * 100)

            if progress_callback:
                progress_callback(progress_percent)
            # --- 结束进度部分 ---

            if current_cost_state_raw is None:
                previous_cost_state_raw = None
                continue

            if previous_cost_state_raw is not None and total_bar_width > 0:
                # 使用相对阈值更稳健
                if previous_cost_state_raw > total_bar_width * 0.9 and \
                        current_cost_state_raw < total_bar_width * 0.1:
                    cost_bar_resets += 1
                    is_collecting_cycle = True
                    print(f"\n检测到费用条重置，已完成 {cost_bar_resets}/{num_cycles} 个循环。")

            if is_collecting_cycle:
                collected_pixel_widths.append(current_cost_state_raw)

            previous_cost_state_raw = current_cost_state_raw

        except Exception as e:
            print(f"\r校准过程中发生错误: {e}. 请检查模拟器状态。重试中...", end="")
            time.sleep(1)
            previous_cost_state_raw = None

    print("\n数据收集完成！开始处理校准数据。")

    # 数据处理部分保持不变...
    unique_pixel_widths = sorted(list(set(collected_pixel_widths)))
    filtered_widths = []
    if unique_pixel_widths:
        filtered_widths.append(unique_pixel_widths[0])
        for i in range(1, len(unique_pixel_widths)):
            # 过滤掉过于接近的像素点，减少抖动带来的噪声
            if unique_pixel_widths[i] - filtered_widths[-1] > 0:  # 至少变化1个像素
                filtered_widths.append(unique_pixel_widths[i])

    pixel_to_frame_map = {}
    total_frames = len(filtered_widths)
    if total_frames < 10:  # 如果帧数太少，校准可能无效
        raise RuntimeError(f"未能收集到足够（{total_frames}）的费用条状态，请确保游戏处于慢速模式并重试。")

    print(f"识别到 {total_frames} 个独特的费用条状态。")
    for i, pixel_width in enumerate(filtered_widths):
        pixel_to_frame_map[str(pixel_width)] = i

    calibrated_data = {
        'pixel_map': pixel_to_frame_map,
        'total_frames': total_frames,
        'screen_width': width,
        'screen_height': height
    }
    print("校准完成！")
    return calibrated_data