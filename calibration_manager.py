import json
import logging
import time
import os
import glob
from typing import Dict, Any, List, Tuple, Optional, Callable
from collections import Counter
import statistics

from PIL import Image

from controllers.base import BaseCaptureController
from utils import find_cost_bar_roi, _get_raw_filled_pixel_width

logger = logging.getLogger(__name__)

CALIBRATION_DIR = "calibration"


def _ensure_cal_dir_exists():
    """确保校准目录存在"""
    if not os.path.exists(CALIBRATION_DIR):
        logger.info(f"校准目录 '{CALIBRATION_DIR}' 不存在，正在创建...")
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
    logger.info(f"正在保存校准数据到 '{filepath}'...")
    logger.debug(f"保存的数据内容: {data}")
    try:
        with open(filepath, 'w', encoding='utf-8') as f:
            json.dump(data, f, indent=4)
        logger.info(f"校准数据已成功保存。")
    except Exception as e:
        logger.exception(f"保存校准文件 '{filepath}' 时发生错误。")
    return filename


def remove_calibration_file(filename: str) -> bool:
    """根据文件名移除校准文件。"""
    filepath = os.path.join(CALIBRATION_DIR, filename)
    logger.info(f"请求移除校准文件: '{filepath}'")
    try:
        os.remove(filepath)
        logger.info(f"已成功移除校准文件。")
        return True
    except FileNotFoundError:
        logger.warning(f"未找到校准文件 '{filepath}'，无需移除。")
        return False
    except Exception as e:
        logger.exception(f"移除校准文件 '{filepath}' 时出错。")
        return False


def load_calibration_by_filename(filename: str) -> Optional[Dict[str, Any]]:
    """通过完整文件名加载校准数据。"""
    filepath = os.path.join(CALIBRATION_DIR, filename)
    logger.info(f"尝试加载校准数据: '{filepath}'")
    try:
        with open(filepath, 'r', encoding='utf-8') as f:
            data = json.load(f)
            required_keys = ['pixel_map', 'total_frames', 'screen_width', 'screen_height']
            if all(k in data for k in required_keys):
                logger.info(f"成功加载并验证校准数据。")
                logger.debug(f"加载的数据: total_frames={data['total_frames']}, resolution={data['screen_width']}x{data['screen_height']}")
                return data
            else:
                missing_keys = [k for k in required_keys if k not in data]
                logger.error(f"校准文件 '{filepath}' 缺少关键数据: {missing_keys}")
                return None
    except FileNotFoundError:
        logger.warning(f"校准文件 '{filepath}' 未找到。")
        return None
    except json.JSONDecodeError:
        logger.error(f"校准文件 '{filepath}' 格式损坏，无法解析JSON。")
        return None
    except Exception as e:
        logger.exception(f"加载校准文件 '{filepath}' 时发生未知错误。")
        return None


def get_calibration_profiles() -> List[Dict[str, Any]]:
    """扫描校准目录，返回所有配置文件的信息列表。"""
    _ensure_cal_dir_exists()
    profiles = []
    logger.debug(f"正在扫描目录 '{CALIBRATION_DIR}' 中的校准配置文件...")
    for filepath in glob.glob(os.path.join(CALIBRATION_DIR, "*.json")):
        filename = os.path.basename(filepath)
        try:
            with open(filepath, 'r', encoding='utf-8') as f:
                data = json.load(f)
                profile_info = {
                    "filename": filename,
                    "basename": get_calibration_basename(filename),
                    "total_frames": data.get("total_frames", "N/A"),
                    "resolution": f"{data.get('screen_width', '?')}x{data.get('screen_height', '?')}"
                }
                profiles.append(profile_info)
                logger.debug(f"找到有效配置文件: {profile_info}")
        except (json.JSONDecodeError, KeyError) as e:
            profile_info = {
                "filename": filename,
                "basename": filename.replace(".json", ""),
                "total_frames": "损坏",
                "resolution": "未知"
            }
            profiles.append(profile_info)
            logger.warning(f"找到损坏的配置文件: {filename}, 错误: {e}")
    sorted_profiles = sorted(profiles, key=lambda p: p['filename'])
    logger.info(f"共找到 {len(sorted_profiles)} 个校准配置文件。")
    return sorted_profiles


def calibrate(controller: BaseCaptureController, num_cycles: int = 5,
              progress_callback: Optional[Callable[[float], None]] = None) -> Dict[str, Any]:
    """
    进行费用条校准。
    """
    logger.info(f"开始费用条校准，目标循环次数: {num_cycles}。")
    collected_pixel_widths: List[int] = []
    current_cost_state_raw: Optional[int] = None
    previous_cost_state_raw: Optional[int] = None
    cost_bar_resets = 0
    is_collecting_cycle = False
    calibration_frame_count = 0

    frame = controller.capture_frame()
    width, height = frame.size
    logger.info(f"校准基于分辨率: {width}x{height}")

    logger.info("开始收集费用条数据...")
    while cost_bar_resets < num_cycles:
        try:
            frame = controller.capture_frame()
            calibration_frame_count += 1
            logger.debug(f"校准捕获第 {calibration_frame_count} 帧。")

            roi = find_cost_bar_roi(width, height)
            current_cost_state_raw = _get_raw_filled_pixel_width(
                frame, roi,
                dump_prefix=f"calib_frame_{calibration_frame_count}"
            )

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
                logger.debug("当前帧未检测到费用条，重置上一帧状态。")
                previous_cost_state_raw = None
                continue

            if previous_cost_state_raw is not None and total_bar_width > 0:
                # 检查是否从高位（如90%以上）跳变到低位（10%以下），标志着一次重置
                if previous_cost_state_raw > total_bar_width * 0.9 and \
                        current_cost_state_raw < total_bar_width * 0.1:
                    cost_bar_resets += 1
                    is_collecting_cycle = True
                    logger.info(f"检测到费用条重置，已完成 {cost_bar_resets}/{num_cycles} 个循环。")

            if is_collecting_cycle:
                logger.debug(f"收集中... 当前原始宽度: {current_cost_state_raw}")
                collected_pixel_widths.append(current_cost_state_raw)

            previous_cost_state_raw = current_cost_state_raw

        except Exception as e:
            logger.exception(f"校准过程中发生错误: {e}. 将在1秒后重试...")
            time.sleep(1)
            previous_cost_state_raw = None # 发生错误后重置状态

    logger.info("数据收集完成！开始处理校准数据。")

    # --- [核心修改] 统计分析模块 ---
    if not collected_pixel_widths:
        logger.error("未能收集到任何有效的费用条状态。")
        raise RuntimeError("未能收集到任何有效的费用条状态，请确保模拟器和游戏运行正常。")

    # 1. 频率统计
    width_counts = Counter(collected_pixel_widths)
    logger.debug(f"收集到的原始宽度频率统计 (前10): {width_counts.most_common(10)}")

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
        logger.debug(f"非零宽度样本数中位数: {median_count}, 离群值阈值: {outlier_threshold}")

        if filtered_counts:
            # c. 用过滤后的数据计算一个更可靠的基准频率（再次使用中位数）
            baseline_frequency = statistics.median(filtered_counts)
            logger.info(f"统计分析: 基准频率 ≈ {baseline_frequency:.2f} 样本/帧")

            # 3. 定量计算隐藏帧数量
            if baseline_frequency > 0:
                # 0像素宽度状态代表的帧数 = 其总样本数 / 基准频率
                num_frames_in_empty_state = round(count_zero / baseline_frequency)
                # 隐藏帧数 = 0像素状态的总帧数 - 1 (因为0本身也算一帧)
                num_hidden_frames = max(0, num_frames_in_empty_state - 1)
                if num_hidden_frames > 0:
                    logger.warning(f"检测到 {num_hidden_frames} 个隐藏的辉光帧，总帧数将自动修正。")
        else:
            logger.warning("未找到稳定的非零样本频率，无法进行隐藏帧检测。")
    else:
        logger.warning("未收集到任何非零的费用条状态。")

    # 4. 构建校准图
    unique_pixel_widths = sorted(width_counts.keys())
    logger.debug(f"共有 {len(unique_pixel_widths)} 个独特的原始宽度值。")

    pixel_to_frame_map = {}
    # 总帧数 = 可见的独特状态数 + 推断出的隐藏帧数
    total_frames = len(unique_pixel_widths) + num_hidden_frames

    # 映射第0帧（即0像素宽度）
    if 0 in unique_pixel_widths:
        pixel_to_frame_map[str(0)] = 0
        logger.debug("映射: 像素宽度 0 -> 逻辑帧 0")

    # 映射后续帧，为隐藏帧留出位置
    frame_offset = 1 + num_hidden_frames
    # 从第一个非零宽度开始映射
    non_zero_widths = [w for w in unique_pixel_widths if w > 0]
    for i, pixel_width in enumerate(non_zero_widths):
        logical_frame = i + frame_offset
        pixel_to_frame_map[str(pixel_width)] = logical_frame
        logger.debug(f"映射: 像素宽度 {pixel_width} -> 逻辑帧 {logical_frame}")
    # --- [结束修改] ---

    if total_frames < 10:
        logger.error(f"校准失败：未能收集到足够的费用条状态 (仅 {total_frames} 帧)。")
        raise RuntimeError(f"未能收集到足够（{total_frames}）的费用条状态，请确保游戏处于慢速模式并重试。")

    logger.info(f"识别到 {total_frames} 个独特的费用条状态（已修正）。")

    calibrated_data = {
        'pixel_map': pixel_to_frame_map,
        'total_frames': total_frames,
        'screen_width': width,
        'screen_height': height,
    }
    logger.info("校准成功完成！")
    return calibrated_data