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
from utils import find_cost_bar_roi, get_logical_frame_from_calibration, _get_raw_filled_pixel_width

logger = logging.getLogger(__name__)

CALIBRATION_DIR = "calibration"


def _calculate_jaccard_similarity(set1: set, set2: set) -> float:
    """计算两个集合的Jaccard相似度"""
    if not set1 and not set2:
        return 1.0
    if not set1 or not set2:
        return 0.0
    intersection_size = len(set1.intersection(set2))
    union_size = len(set1.union(set2))
    return intersection_size / union_size


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
    # 从新的多模型结构中获取总帧数信息用于文件名
    profiles = data.get('profiles', [])
    if profiles:
        # 文件名中可以包含多个周期的帧数，例如 37f-38f
        frame_counts_str = "-".join(str(p['total_frames']) for p in profiles) + "f"
    else:
        frame_counts_str = "0f"

    data['calibration_time'] = time.time()
    filename = f"{basename}_{frame_counts_str}_{screen_width}x{screen_height}.json"
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
            # 检查新/旧格式
            is_new_format = 'profiles' in data and isinstance(data['profiles'], list)
            is_old_format = 'pixel_map' in data

            if is_new_format:
                logger.info("检测到新的多模型校准格式。")
                if all('total_frames' in p and 'pixel_map' in p for p in data['profiles']):
                    logger.info("多模型校准数据验证成功。")
                    return data
                else:
                    logger.error(f"校准文件 '{filepath}' 的多模型格式不完整。")
                    return None
            elif is_old_format:
                logger.warning(f"检测到旧的单模型校准格式。将进行兼容性转换。")
                # 兼容性转换：将旧格式包装成新的多模型格式
                transformed_data = {
                    "detection_mode": "single",
                    "profiles": [{
                        "total_frames": data.get("total_frames"),
                        "pixel_map": data.get("pixel_map")
                    }],
                    "screen_width": data.get("screen_width"),
                    "screen_height": data.get("screen_height"),
                    "calibration_time": data.get("calibration_time")
                }
                logger.info("旧格式已成功转换为新格式。")
                return transformed_data
            else:
                logger.error(f"校准文件 '{filepath}' 格式无法识别，既不包含 'profiles' 也不包含 'pixel_map'。")
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
    profiles_info = []
    logger.debug(f"正在扫描目录 '{CALIBRATION_DIR}' 中的校准配置文件...")
    for filepath in glob.glob(os.path.join(CALIBRATION_DIR, "*.json")):
        filename = os.path.basename(filepath)
        try:
            with open(filepath, 'r', encoding='utf-8') as f:
                data = json.load(f)

                # 兼容新旧两种格式
                if 'profiles' in data and isinstance(data.get('profiles'), list):
                    frame_counts = [p.get('total_frames', 'N/A') for p in data['profiles']]
                    total_frames_str = "-".join(map(str, frame_counts)) + "f"
                else:
                    total_frames_str = str(data.get("total_frames", "N/A")) + "f"

                profile_info = {
                    "filename": filename,
                    "basename": get_calibration_basename(filename),
                    "total_frames_str": total_frames_str,
                    "resolution": f"{data.get('screen_width', '?')}x{data.get('screen_height', '?')}"
                }
                profiles_info.append(profile_info)
                logger.debug(f"找到有效配置文件: {profile_info}")
        except (json.JSONDecodeError, KeyError) as e:
            profile_info = {
                "filename": filename,
                "basename": filename.replace(".json", ""),
                "total_frames_str": "损坏",
                "resolution": "未知"
            }
            profiles_info.append(profile_info)
            logger.warning(f"找到损坏的配置文件: {filename}, 错误: {e}")

    sorted_profiles = sorted(profiles_info, key=lambda p: p['filename'])
    logger.info(f"共找到 {len(sorted_profiles)} 个校准配置文件。")
    return sorted_profiles


def calibrate(controller: BaseCaptureController, num_cycles: int = 6,
              progress_callback: Optional[Callable[[float], None]] = None) -> Dict[str, Any]:
    """
    进行费用条校准，现在支持多模型检测和聚类。
    """
    logger.info(f"开始费用条校准，目标循环次数: {num_cycles}。")
    cycle_samples: List[List[int]] = []
    current_cycle_data: List[int] = []

    current_cost_state_raw: Optional[int] = None
    previous_cost_state_raw: Optional[int] = None
    is_collecting_cycle = False
    calibration_frame_count = 0

    frame = controller.capture_frame()
    width, height = frame.size
    logger.info(f"校准基于分辨率: {width}x{height}")

    logger.info("开始收集费用条循环样本...")
    while len(cycle_samples) < num_cycles:
        try:
            frame = controller.capture_frame()
            calibration_frame_count += 1

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

            overall_progress = (len(cycle_samples) + current_fill_percentage) / num_cycles
            progress_percent = min(100.0, overall_progress * 100)

            if progress_callback:
                progress_callback(progress_percent)

            if current_cost_state_raw is None:
                previous_cost_state_raw = None
                continue

            if previous_cost_state_raw is not None and total_bar_width > 0:
                if previous_cost_state_raw > total_bar_width * 0.9 and \
                        current_cost_state_raw < total_bar_width * 0.1:
                    is_collecting_cycle = True
                    if current_cycle_data:
                        cycle_samples.append(current_cycle_data)
                        logger.info(
                            f"收集到一个完整的循环样本 (包含 {len(current_cycle_data)} 帧数据)，已完成 {len(cycle_samples)}/{num_cycles} 个循环。")
                        current_cycle_data = []  # 重置

            if is_collecting_cycle:
                current_cycle_data.append(current_cost_state_raw)

            previous_cost_state_raw = current_cost_state_raw

        except Exception as e:
            logger.exception(f"校准过程中发生错误: {e}. 将在1秒后重试...")
            time.sleep(1)
            previous_cost_state_raw = None

    logger.info("数据收集完成！开始聚类和建模。")

    # --- [核心修改] 基于Jaccard相似度的内容聚类 ---
    if not cycle_samples:
        raise RuntimeError("未能收集到任何有效的费用条循环，请确保游戏处于慢速模式并重试。")

    clusters: List[List[List[int]]] = []
    SIMILARITY_THRESHOLD = 0.8  # 相似度阈值

    for sample in cycle_samples:
        sample_set = set(sample)
        if not sample_set: continue

        best_match_cluster_index = -1
        max_similarity = -1

        for i, cluster in enumerate(clusters):
            # 使用簇的第一个样本作为代表进行比较
            representative_set = set(cluster[0])
            similarity = _calculate_jaccard_similarity(sample_set, representative_set)

            if similarity > max_similarity:
                max_similarity = similarity
                best_match_cluster_index = i

        if max_similarity >= SIMILARITY_THRESHOLD:
            logger.debug(f"样本与簇 {best_match_cluster_index} 相似度为 {max_similarity:.2f}，加入该簇。")
            clusters[best_match_cluster_index].append(sample)
        else:
            logger.info(f"未找到足够相似的簇 (最高相似度 {max_similarity:.2f})，创建新簇。")
            clusters.append([sample])

    logger.info(f"聚类完成，共形成 {len(clusters)} 个不同的费用循环模型。")

    # --- 为每个簇独立建模 ---
    final_profiles = []
    for i, cluster in enumerate(clusters):
        logger.info(f"--- 正在为第 {i + 1} 个模型（包含 {len(cluster)} 个样本）进行分析 ---")

        # 合并簇内所有样本数据
        merged_widths = [width for sample in cluster for width in sample]
        width_counts = Counter(merged_widths)

        # 统计分析（离群值排除和隐藏帧检测）
        count_zero = width_counts.get(0, 0)
        non_zero_counts = [count for width, count in width_counts.items() if width > 0]
        num_hidden_frames = 0
        if non_zero_counts:
            median_count = statistics.median(non_zero_counts)
            outlier_threshold = median_count * 5
            filtered_counts = [count for count in non_zero_counts if count < outlier_threshold]
            if filtered_counts:
                baseline_frequency = statistics.median(filtered_counts)
                logger.info(f"模型 {i + 1}: 基准频率 ≈ {baseline_frequency:.2f} 样本/帧")
                if baseline_frequency > 0:
                    num_frames_in_empty_state = round(count_zero / baseline_frequency)
                    num_hidden_frames = max(0, num_frames_in_empty_state - 1)
                    if num_hidden_frames > 0:
                        logger.warning(f"模型 {i + 1}: 检测到 {num_hidden_frames} 个隐藏辉光帧。")
            else:
                logger.warning(f"模型 {i + 1}: 未找到稳定频率，无法检测隐藏帧。")
        else:
            logger.warning(f"模型 {i + 1}: 未收集到任何非零状态。")

        # 构建校准图
        unique_pixel_widths = sorted(width_counts.keys())
        pixel_to_frame_map = {}
        total_frames = len(unique_pixel_widths) + num_hidden_frames

        if 0 in unique_pixel_widths:
            pixel_to_frame_map[str(0)] = 0

        frame_offset = 1 + num_hidden_frames
        non_zero_widths = [w for w in unique_pixel_widths if w > 0]
        for idx, pixel_width in enumerate(non_zero_widths):
            logical_frame = idx + frame_offset
            pixel_to_frame_map[str(pixel_width)] = logical_frame

        final_profiles.append({
            "total_frames": total_frames,
            "pixel_map": pixel_to_frame_map
        })
        logger.info(f"模型 {i + 1} 构建完成，总帧数: {total_frames}。")

    if not final_profiles:
        raise RuntimeError("校准失败：未能构建任何有效的费用循环模型。")

    calibrated_data = {
        "detection_mode": "alternating" if len(final_profiles) > 1 else "single",
        "profiles": final_profiles,
        "screen_width": width,
        "screen_height": height,
    }
    logger.info("校准成功完成！")
    return calibrated_data