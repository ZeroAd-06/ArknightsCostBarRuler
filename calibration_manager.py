import json
import time
import os
import glob
from typing import Dict, Any, List, Tuple, Optional, Callable

from PIL import Image

from controllers.base import BaseCaptureController
from utils import find_cost_bar_roi, _get_raw_filled_pixel_width

CALIBRATION_DIR = "calibration"

def _ensure_cal_dir_exists():
    """确保校准目录存在"""
    os.makedirs(CALIBRATION_DIR, exist_ok=True)

def get_calibration_basename(filename: str) -> str:
    """从完整文件名中提取基础名称部分"""
    return filename.split('_')[0]

def save_calibration_data(data: Dict[str, Any], screen_width: int, screen_height: int, basename: str) -> str:
    """
    保存校准数据到文件，并返回完整的文件名。
    文件名格式: {basename}_{total_frames}f_{w}x{h}.json
    """
    _ensure_cal_dir_exists()
    total_frames = data.get('total_frames', 0)
    # 添加时间戳
    data['calibration_time'] = time.time()
    filename = f"{basename}_{total_frames}f_{screen_width}x{screen_height}.json"
    filepath = os.path.join(CALIBRATION_DIR, filename)
    with open(filepath, 'w', encoding='utf-8') as f:
        json.dump(data, f, indent=4)
    print(f"\n校准数据已保存到 '{filepath}'")
    return filename

def remove_calibration_file(filename: str) -> bool:
    """根据文件名移除校准文件。"""
    filepath = os.path.join(CALIBRATION_DIR, filename)
    try:
        os.remove(filepath)
        print(f"已移除校准文件: '{filepath}'")
        return True
    except FileNotFoundError:
        print(f"未找到校准文件 '{filepath}'，无需移除。")
        return False
    except Exception as e:
        print(f"移除校准文件时出错: {e}")
        return False

def load_calibration_by_filename(filename: str) -> Optional[Dict[str, Any]]:
    """通过完整文件名加载校准数据。"""
    filepath = os.path.join(CALIBRATION_DIR, filename)
    try:
        with open(filepath, 'r', encoding='utf-8') as f:
            data = json.load(f)
            # 基本验证
            if all(k in data for k in ['pixel_map', 'total_frames', 'screen_width', 'screen_height']):
                print(f"已加载校准数据: '{filepath}'")
                return data
            else:
                 print(f"警告: 校准文件 '{filepath}' 缺少关键数据。")
                 return None
    except FileNotFoundError:
        return None
    except json.JSONDecodeError:
        print(f"错误: 校准文件 '{filepath}' 格式损坏。")
        return None

def get_calibration_profiles() -> List[Dict[str, Any]]:
    """扫描校准目录，返回所有配置文件的信息列表。"""
    _ensure_cal_dir_exists()
    profiles = []
    for filepath in glob.glob(os.path.join(CALIBRATION_DIR, "*.json")):
        filename = os.path.basename(filepath)
        try:
            with open(filepath, 'r', encoding='utf-8') as f:
                data = json.load(f)
                profiles.append({
                    "filename": filename,
                    "basename": get_calibration_basename(filename),
                    "total_frames": data.get("total_frames", "N/A"),
                    "resolution": f"{data.get('screen_width', '?')}x{data.get('screen_height', '?')}"
                })
        except (json.JSONDecodeError, KeyError):
            # 如果文件损坏或格式不对，也显示出来，方便用户删除
            profiles.append({
                "filename": filename,
                "basename": filename.replace(".json", ""),
                "total_frames": "损坏",
                "resolution": "未知"
            })
    return sorted(profiles, key=lambda p: p['filename'])


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

    unique_pixel_widths = sorted(list(set(collected_pixel_widths)))
    filtered_widths = []
    if unique_pixel_widths:
        filtered_widths.append(unique_pixel_widths[0])
        for i in range(1, len(unique_pixel_widths)):
            if unique_pixel_widths[i] - filtered_widths[-1] > 0:
                filtered_widths.append(unique_pixel_widths[i])

    pixel_to_frame_map = {}
    total_frames = len(filtered_widths)
    if total_frames < 10:
        raise RuntimeError(f"未能收集到足够（{total_frames}）的费用条状态，请确保游戏处于慢速模式并重试。")

    print(f"识别到 {total_frames} 个独特的费用条状态。")
    for i, pixel_width in enumerate(filtered_widths):
        pixel_to_frame_map[str(pixel_width)] = i

    calibrated_data = {
        'pixel_map': pixel_to_frame_map,
        'total_frames': total_frames,
        'screen_width': width,
        'screen_height': height,
    }
    print("校准完成！")
    return calibrated_data