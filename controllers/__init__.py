import logging
from typing import Dict, Any, List

from .base import BaseCaptureController

logger = logging.getLogger(__name__)

# 定义所有已知的明日方舟包名
KNOWN_PACKAGE_NAMES: List[str] = [
    "com.hypergryph.arknights",  # 官服
    "com.hypergryph.arknights.bilibili",  # Bilibili 服
    "com.YoStarJP.Arknights",  # 日服
    "com.YoStarEN.Arknights",  # 国际服 (英文)
    "com.YoStarKR.Arknights",  # 韩服
    "tw.txwy.and.arknights"  # 台服
]


def create_capture_controller(config: Dict[str, Any]) -> BaseCaptureController:
    """
    根据配置创建并返回一个合适的截图控制器实例。

    Args:
        config (Dict[str, Any]): 包含控制器类型和其所需参数的配置字典。

    Returns:
        BaseCaptureController: 一个控制器实例。

    Raises:
        ValueError: 如果配置中的 'type' 不被支持或缺少必要参数。
    """
    controller_type = config.get("type")
    if not controller_type:
        logger.error("创建控制器失败：配置字典中缺少 'type' 键。")
        raise ValueError("配置字典中必须包含 'type' 键。")

    logger.info(f"根据配置创建 '{controller_type}' 控制器...")
    logger.debug(f"完整配置: {config}")

    if controller_type == "mumu":
        from .mumu import MuMuPlayerController
        install_path = config.get("install_path")
        if not install_path:
            logger.error("类型为 'mumu' 的配置必须包含 'install_path'。")
            raise ValueError("类型为 'mumu' 的配置必须包含 'install_path'。")

        # 从配置中获取实例索引，如果不存在则默认为 0
        instance_index = config.get("instance_index", 0)

        logger.debug(f"创建 MuMuPlayerController, install_path='{install_path}', instance_index={instance_index}")
        return MuMuPlayerController(
            mumu_install_path=install_path,
            instance_index=instance_index,
            package_name_list=KNOWN_PACKAGE_NAMES  # 传递包名列表
        )

    elif controller_type == "minicap":
        from .minicap import MinicapController
        device_id = config.get("device_id")
        logger.debug(f"创建 MinicapController, device_id='{device_id}'")
        return MinicapController(device_id=device_id)

    else:
        logger.error(f"不支持的控制器类型: '{controller_type}'")
        raise ValueError(f"不支持的控制器类型: '{controller_type}'")

