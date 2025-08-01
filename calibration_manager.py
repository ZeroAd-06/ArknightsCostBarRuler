import json
import time
import os
import glob
from typing import Dict, Any, List, Tuple, Optional, Callable
from collections import Counter
import statistics

from PIL import Image

from controllers.base import BaseCaptureController
from utils import find_cost_bar_roi, _get_raw_filled_pixel_width

CALIBRATION_DIR = "calibration"


def _ensure_cal_dir_exists():
    """确保校准目录存在"""
    os.makedirs(CALIBRATION_DIR, exist_ok=True)


def get_calibration_basename(filename: str) -> str:
    """从完整文件名中提取基础名称部分"""
    return filename.split('_')[0] if '_' in filename else filename.replace(".json", "")


def save_calibration_data(data: Dict[str, Any], screen_width: int, screen_height: int, basename: str) -> str:
    """保存校准数据到文件。"""
    _ensure_cal_dir_exists()
    total_frames = data.get('total_frames', 0)
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
    进行费用条校准。
    """

    collected_pixel_widths: List[int] = []
    current_cost_state_raw: Optional[int] = None
    previous_cost_state_raw: Optional[int] = None
    cost_bar_resets = 0
    is_collecting_cycle = False

    frame = controller.capture_frame()
    width, height = frame.size

    print("\n开始收集数据...")
    while cost_bar_resets < num_cycles:
        try:
            frame = controller.capture_frame()
            roi = find_cost_bar_roi(width, height)
            current_cost_state_raw = _get_raw_filled_pixel_width(frame, roi)

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

    # --- [核心修改] 统计分析模块 ---
    if not collected_pixel_widths:
        raise RuntimeError("未能收集到任何有效的费用条状态，请确保模拟器和游戏运行正常。")

    # 1. 频率统计
    width_counts = Counter(collected_pixel_widths)

    # 2. 离群值排除与基准频率计算
    count_zero = width_counts.get(0, 0)
    non_zero_counts = [count for width, count in width_counts.items() if width > 0]
    num_hidden_frames = 0

    if non_zero_counts:
        # a. 使用中位数来初步抵抗离群值
        median_count = statistics.median(non_zero_counts)
        # b. 定义一个离群值阈值（中位数的5倍），过滤掉因暂停导致的极端值
        outlier_threshold = median_count * 5
        filtered_counts = [count for count in non_zero_counts if count < outlier_threshold]

        if filtered_counts:
            # c. 用过滤后的数据计算一个更可靠的基准频率（再次使用中位数）
            baseline_frequency = statistics.median(filtered_counts)
            print(f"统计分析: 基准频率 ≈ {baseline_frequency:.2f} 样本/帧")

            # 3. 定量计算隐藏帧数量
            if baseline_frequency > 0:
                # 0像素宽度状态代表的帧数 = 其总样本数 / 基准频率
                num_frames_in_empty_state = round(count_zero / baseline_frequency)
                num_hidden_frames = max(0, num_frames_in_empty_state - 1)
                if num_hidden_frames > 0:
                    print(f"检测到 {num_hidden_frames} 个隐藏的辉光帧，总帧数将自动修正。")
        else:
            print("警告: 未找到稳定的非零样本频率，无法进行隐藏帧检测。")
    else:
        print("警告: 未收集到任何非零的费用条状态。")

    # 4. 构建校准图
    unique_pixel_widths = sorted(width_counts.keys())

    pixel_to_frame_map = {}
    # 总帧数 = 可见的独特状态数 + 推断出的隐藏帧数
    total_frames = len(unique_pixel_widths) + num_hidden_frames

    # 映射第0帧（即0像素宽度）
    if 0 in unique_pixel_widths:
        pixel_to_frame_map[str(0)] = 0

    # 映射后续帧，为隐藏帧留出位置
    frame_offset = 1 + num_hidden_frames
    # 从第一个非零宽度开始映射
    non_zero_widths = [w for w in unique_pixel_widths if w > 0]
    for i, pixel_width in enumerate(non_zero_widths):
        pixel_to_frame_map[str(pixel_width)] = i + frame_offset
    # --- [结束修改] ---

    if total_frames < 10:
        raise RuntimeError(f"未能收集到足够（{total_frames}）的费用条状态，请确保游戏处于慢速模式并重试。")

    print(f"识别到 {total_frames} 个独特的费用条状态（已修正）。")

    calibrated_data = {
        'pixel_map': pixel_to_frame_map,
        'total_frames': total_frames,
        'screen_width': width,
        'screen_height': height,
    }
    print("校准完成！")
    return calibrated_data