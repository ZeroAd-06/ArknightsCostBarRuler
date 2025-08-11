import json
import logging
from tkinter import filedialog

import ttkbootstrap as ttk
from ttkbootstrap.dialogs import Messagebox
from typing import Dict, Any, Optional

logger = logging.getLogger(__name__)

CONFIG_FILE = "../config.json"


def load_config() -> Optional[Dict[str, Any]]:
    """加载配置文件"""
    logger.info(f"尝试从 '{CONFIG_FILE}' 加载配置...")
    try:
        with open(CONFIG_FILE, 'r', encoding='utf-8') as f:
            config = json.load(f)
            if config:
                logger.info("配置加载成功。")
                logger.debug(f"加载的配置内容: {config}")
                return config
            logger.warning(f"配置文件 '{CONFIG_FILE}' 为空。")
            return None
    except FileNotFoundError:
        logger.info(f"配置文件 '{CONFIG_FILE}' 未找到。")
        return None
    except json.JSONDecodeError:
        logger.error(f"配置文件 '{CONFIG_FILE}' 格式损坏。")
        return None
    except Exception as e:
        logger.exception(f"加载配置文件时发生未知错误: {e}")
        return None


def save_config(config: Dict[str, Any]):
    """保存配置文件"""
    logger.info(f"正在保存配置到 '{CONFIG_FILE}'...")
    logger.debug(f"待保存的配置内容: {config}")
    try:
        with open(CONFIG_FILE, 'w', encoding='utf-8') as f:
            json.dump(config, f, indent=4, ensure_ascii=False)
        logger.info("配置保存成功。")
    except Exception as e:
        logger.exception(f"保存配置文件时发生错误: {e}")


class ConfigWindow(ttk.Toplevel):
    """
    一个使用 ttkbootstrap 风格的对话框窗口，用于引导用户完成首次配置。
    """

    def __init__(self, parent):
        super().__init__(parent)
        logger.debug("初始化首次配置向导窗口 (ConfigWindow)...")

        self.config_data = None
        self.FONT_NORMAL = ("Microsoft YaHei UI", 10)
        self.title("首次使用配置向导")
        self.grab_set()  # 模态窗口

        # --- 核心修改：使用字典管理模拟器选项 ---
        self.EMULATOR_OPTIONS = {
            "MuMu模拟器12": "mumu",
            "雷电模拟器": "ldplayer",
            "其他 (通用ADB)": "minicap"
        }
        self.selected_display_name = ttk.StringVar(master=self)

        main_frame = ttk.Frame(self, padding="15 15 15 15")
        main_frame.pack(expand=True, fill=ttk.BOTH)

        self._create_widgets(main_frame)
        self._on_selection_change()  # 初始化显示正确的设置
        self.update_idletasks()
        self.center_on_screen()

        self.resizable(False, False)
        logger.debug("ConfigWindow 初始化完成。")

    def center_on_screen(self):
        """将窗口置于屏幕中央。"""
        logger.debug("正在将配置窗口居中...")
        width = self.winfo_width()
        height = self.winfo_height()
        screen_width = self.winfo_screenwidth()
        screen_height = self.winfo_screenheight()
        x = (screen_width // 2) - (width // 2)
        y = (screen_height // 2) - (height // 2)
        self.geometry(f"{width}x{height}+{x}+{y}")
        logger.debug(f"窗口位置设置为: {x}, {y}")

    def _create_widgets(self, parent: ttk.Frame):
        logger.debug("正在创建 ConfigWindow 的控件...")
        parent.columnconfigure(0, weight=1)

        header_label = ttk.Label(parent, text="首次使用，请完成连接配置。", font=("Microsoft YaHei UI", 14, "bold"))
        header_label.grid(row=0, column=0, pady=(0, 20), sticky="w")

        # --- 核心修改：使用下拉框代替单选按钮 ---
        type_frame = ttk.Labelframe(parent, text=" 模拟器类型 ")
        type_frame.grid(row=1, column=0, sticky="ew", pady=10)
        type_frame.columnconfigure(0, weight=1)

        self.emulator_combobox = ttk.Combobox(
            type_frame,
            textvariable=self.selected_display_name,
            values=list(self.EMULATOR_OPTIONS.keys()),
            state="readonly"
        )
        self.emulator_combobox.grid(row=0, column=0, padx=10, pady=10, sticky="ew")
        self.emulator_combobox.set(list(self.EMULATOR_OPTIONS.keys())[0])  # 默认选中第一个
        self.emulator_combobox.bind("<<ComboboxSelected>>", self._on_selection_change)
        # --- 修改结束 ---

        self.options_container = ttk.Frame(parent)
        self.options_container.grid(row=2, column=0, sticky="ew", pady=5)
        self.options_container.columnconfigure(0, weight=1)

        # 创建所有可能的设置框架
        self.mumu_frame = self._create_mumu_frame(self.options_container)
        self.mumu_frame.grid(row=0, column=0, sticky="nsew")

        self.ldplayer_frame = self._create_ldplayer_frame(self.options_container)
        self.ldplayer_frame.grid(row=0, column=0, sticky="nsew")

        self.minicap_frame = self._create_minicap_frame(self.options_container)
        self.minicap_frame.grid(row=0, column=0, sticky="nsew")

        save_button = ttk.Button(parent, text="保存并启动", command=self._save_and_close, bootstyle="success")
        save_button.grid(row=3, column=0, pady=(20, 0), ipady=5, sticky="ew")
        logger.debug("控件创建完成。")

    def _create_mumu_frame(self, parent) -> ttk.Frame:
        frame = ttk.Labelframe(parent, text=" MuMu模拟器12 设置 ")
        frame.columnconfigure(1, weight=1)

        path_label = ttk.Label(frame, text="安装路径:", font=self.FONT_NORMAL)
        path_label.grid(row=0, column=0, padx=(10, 5), pady=10, sticky="w")
        self.mumu_path_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.mumu_path_entry.grid(row=0, column=1, padx=5, pady=10, sticky="ew")
        browse_button = ttk.Button(frame, text="浏览...", command=self._browse_mumu_path, bootstyle="secondary-outline")
        browse_button.grid(row=0, column=2, padx=(5, 10), pady=10, sticky="e")

        instance_label = ttk.Label(frame, text="实例索引:", font=self.FONT_NORMAL)
        instance_label.grid(row=1, column=0, padx=(10, 5), pady=(0, 10), sticky="w")
        self.mumu_instance_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.mumu_instance_entry.grid(row=1, column=1, padx=5, pady=(0, 10), sticky="ew", columnspan=2)
        self.mumu_instance_entry.insert(0, "0")
        return frame

    def _create_ldplayer_frame(self, parent) -> ttk.Frame:
        frame = ttk.Labelframe(parent, text=" 雷电模拟器 设置 ")
        frame.columnconfigure(1, weight=1)

        path_label = ttk.Label(frame, text="安装路径:", font=self.FONT_NORMAL)
        path_label.grid(row=0, column=0, padx=(10, 5), pady=10, sticky="w")
        self.ldplayer_path_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.ldplayer_path_entry.grid(row=0, column=1, padx=5, pady=10, sticky="ew")
        browse_button = ttk.Button(frame, text="浏览...", command=self._browse_ldplayer_path,
                                   bootstyle="secondary-outline")
        browse_button.grid(row=0, column=2, padx=(5, 10), pady=10, sticky="e")

        instance_label = ttk.Label(frame, text="实例索引:", font=self.FONT_NORMAL)
        instance_label.grid(row=1, column=0, padx=(10, 5), pady=10, sticky="w")
        self.ldplayer_instance_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.ldplayer_instance_entry.grid(row=1, column=1, padx=5, pady=10, sticky="ew", columnspan=2)
        self.ldplayer_instance_entry.insert(0, "0")

        adb_label = ttk.Label(frame, text="ADB Device ID (可选):", font=self.FONT_NORMAL)
        adb_label.grid(row=2, column=0, padx=(10, 5), pady=(0, 10), sticky="w")
        self.ldplayer_id_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.ldplayer_id_entry.grid(row=2, column=1, padx=5, pady=(0, 10), sticky="ew", columnspan=2)
        return frame

    def _create_minicap_frame(self, parent) -> ttk.Frame:
        frame = ttk.Labelframe(parent, text=" 通用ADB 设置 ")
        frame.columnconfigure(0, weight=1)
        label = ttk.Label(frame, text="ADB Device ID (可选, 留空则自动检测):", font=self.FONT_NORMAL)
        label.grid(row=0, column=0, padx=10, pady=(10, 5), sticky="w")
        self.minicap_id_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.minicap_id_entry.grid(row=1, column=0, padx=10, pady=(0, 10), sticky="ew")
        return frame

    def _on_selection_change(self, event=None):
        display_name = self.selected_display_name.get()
        selected_type = self.EMULATOR_OPTIONS.get(display_name)
        logger.debug(f"模拟器类型切换为: {selected_type}")

        if selected_type == "mumu":
            self.mumu_frame.tkraise()
        elif selected_type == "ldplayer":
            self.ldplayer_frame.tkraise()
        else:  # minicap
            self.minicap_frame.tkraise()

    def _browse_mumu_path(self):
        logger.debug("打开 '浏览' 对话框以选择MuMu路径。")
        path = filedialog.askdirectory(title="请选择MuMu模拟器12的安装根目录")
        if path:
            logger.info(f"用户选择了MuMu路径: {path}")
            self.mumu_path_entry.delete(0, ttk.END)
            self.mumu_path_entry.insert(0, path)
        else:
            logger.debug("用户取消了路径选择。")

    def _browse_ldplayer_path(self):
        logger.debug("打开 '浏览' 对话框以选择雷电模拟器路径。")
        path = filedialog.askdirectory(title="请选择雷电模拟器的安装根目录")
        if path:
            logger.info(f"用户选择了雷电模拟器路径: {path}")
            self.ldplayer_path_entry.delete(0, ttk.END)
            self.ldplayer_path_entry.insert(0, path)
        else:
            logger.debug("用户取消了路径选择。")

    def _save_and_close(self):
        logger.debug("用户点击 '保存并启动' 按钮。")
        display_name = self.selected_display_name.get()
        cfg_type = self.EMULATOR_OPTIONS.get(display_name)

        self.config_data = {"type": cfg_type, "active_calibration_profile": None}

        if cfg_type == "mumu":
            mumu_path = self.mumu_path_entry.get().strip()
            if not mumu_path:
                logger.warning("保存失败：MuMu模拟器安装路径为空。")
                Messagebox.show_error("MuMu模拟器安装路径不能为空！", title="错误", parent=self)
                return
            self.config_data["install_path"] = mumu_path

            instance_str = self.mumu_instance_entry.get().strip()
            try:
                instance_idx = int(instance_str)
            except ValueError:
                logger.warning(f"无效的MuMu实例索引 '{instance_str}'，将使用默认值 0。")
                instance_idx = 0
            self.config_data["instance_index"] = instance_idx

        elif cfg_type == "ldplayer":
            ld_path = self.ldplayer_path_entry.get().strip()
            if not ld_path:
                logger.warning("保存失败：雷电模拟器安装路径为空。")
                Messagebox.show_error("雷电模拟器安装路径不能为空！", title="错误", parent=self)
                return
            self.config_data["install_path"] = ld_path

            instance_str = self.ldplayer_instance_entry.get().strip()
            try:
                instance_idx = int(instance_str)
            except ValueError:
                logger.warning(f"无效的雷电实例索引 '{instance_str}'，将使用默认值 0。")
                instance_idx = 0
            self.config_data["instance_index"] = instance_idx

            ld_id = self.ldplayer_id_entry.get().strip()
            if ld_id:
                self.config_data["device_id"] = ld_id

        else:  # minicap
            minicap_id = self.minicap_id_entry.get().strip()
            if minicap_id:
                self.config_data["device_id"] = minicap_id

        logger.info(f"生成新配置: {self.config_data}")
        save_config(self.config_data)
        self.destroy()
        logger.debug("ConfigWindow 已销毁。")


def create_config_with_gui(parent) -> Optional[Dict[str, Any]]:
    """启动GUI让用户创建配置。"""
    logger.info("启动配置向导 GUI...")
    window = ConfigWindow(parent)
    parent.wait_window(window)  # 等待窗口关闭
    logger.info(f"配置向导结束。返回的配置数据: {window.config_data}")
    return window.config_data
