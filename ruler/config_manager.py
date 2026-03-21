import json
import logging
import sys
import ctypes
from ctypes import wintypes
import tkinter as tk
from tkinter import filedialog

from PIL import Image, ImageTk
import ttkbootstrap as ttk
from ttkbootstrap.dialogs import Messagebox
from typing import Dict, Any, Optional

from i18n import i18n

try:
    from controllers.windows import WindowsWindowController
except ImportError:
    WindowsWindowController = None

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
                
                # Load language preference
                lang = config.get('language')
                if lang:
                     i18n.load_locale(lang)
                else:
                     i18n.load_locale(i18n.auto_detect_language())

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
        self.title(i18n.get("config.window.title"))
        self.grab_set()  # 模态窗口

        # --- 核心修改：使用字典管理连接类型选项 ---
        self.EMULATOR_OPTIONS = {
            i18n.get("config.type.mumu"): "mumu",
            i18n.get("config.type.ldplayer"): "ldplayer",
            i18n.get("config.type.minicap"): "minicap",
            i18n.get("config.type.window"): "window"
        }
        self.selected_display_name = ttk.StringVar(master=self)
        self.selected_window_handle = None
        self.selected_window_title = None
        self.selected_window_class = None
        self.window_items = []
        self._window_preview_photo = None
        self.window_preview_after_id = None

        main_frame = ttk.Frame(self, padding="15 15 15 15")
        main_frame.pack(expand=True, fill="both")

        self._create_widgets(main_frame)
        self._on_selection_change()  # 初始化显示正确的设置
        self.update_idletasks()
        self._adjust_window_size()
        self.center_on_screen()

        self.resizable(True, True)
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

    def _adjust_window_size(self):
        """根据内容自适应窗口大小。"""
        # 先确保内部控件完成测量
        self.update_idletasks()

        # 获取自适应尺寸
        width = max(self.winfo_reqwidth(), 620)
        height = max(self.winfo_reqheight(), 420)
        max_width = int(self.winfo_screenwidth() * 0.9)
        max_height = int(self.winfo_screenheight() * 0.9)
        width = min(width, max_width)
        height = min(height, max_height)

        self.geometry(f"{width}x{height}")
        self.minsize(620, 420)

        # 标签换行长度随窗口宽度调整
        self.header_label.config(wraplength=width - 30)
        self.update_idletasks()

    def _create_widgets(self, parent: ttk.Frame):
        logger.debug("正在创建 ConfigWindow 的控件...")
        parent.columnconfigure(0, weight=1)

        # Language Selection
        lang_frame = ttk.Frame(parent)
        lang_frame.grid(row=0, column=0, pady=(0, 10), sticky="ew")
        
        lang_label = ttk.Label(lang_frame, text=i18n.get("config.language"), font=self.FONT_NORMAL)
        lang_label.pack(side="left", padx=(0, 10))

        self.language_var = ttk.StringVar(value=i18n.current_locale)
        self.language_combo = ttk.Combobox(lang_frame, textvariable=self.language_var, 
                                           values=["zh_CN", "en_US"], state="readonly", width=10)
        self.language_combo.pack(side="left")
        self.language_combo.bind("<<ComboboxSelected>>", self._on_language_change)

        self.header_label = ttk.Label(parent, text=i18n.get("config.header"), font=("Microsoft YaHei UI", 14, "bold"), wraplength=700, justify="left")
        self.header_label.grid(row=1, column=0, pady=(0, 20), sticky="ew")

        # --- 核心修改：使用下拉框代替单选按钮 ---
        self.type_frame = ttk.Labelframe(parent, text=i18n.get("config.emulator_type"))
        self.type_frame.grid(row=2, column=0, sticky="ew", pady=10)
        self.type_frame.columnconfigure(0, weight=1)

        self.emulator_combobox = ttk.Combobox(
            self.type_frame,
            textvariable=self.selected_display_name,
            values=list(self.EMULATOR_OPTIONS.keys()),
            state="readonly"
        )
        self.emulator_combobox.grid(row=0, column=0, padx=10, pady=10, sticky="ew")
        self.emulator_combobox.set(list(self.EMULATOR_OPTIONS.keys())[0])  # 默认选中第一个
        self.emulator_combobox.bind("<<ComboboxSelected>>", self._on_selection_change)

        self.window_frame = self._create_window_frame(parent)
        self.window_frame.grid(row=3, column=0, sticky="ew", pady=5)

        self.options_container = ttk.Frame(parent)
        self.options_container.grid(row=4, column=0, sticky="ew", pady=5)
        self.options_container.columnconfigure(0, weight=1)

        # 创建所有可能的设置框架
        self.mumu_frame = self._create_mumu_frame(self.options_container)
        self.mumu_frame.grid(row=0, column=0, sticky="nsew")

        self.ldplayer_frame = self._create_ldplayer_frame(self.options_container)
        self.ldplayer_frame.grid(row=0, column=0, sticky="nsew")

        self.minicap_frame = self._create_minicap_frame(self.options_container)
        self.minicap_frame.grid(row=0, column=0, sticky="nsew")

        self.save_button = ttk.Button(parent, text=i18n.get("config.btn.save_start"), command=self._save_and_close, bootstyle="success")
        self.save_button.grid(row=5, column=0, pady=(8, 0), ipady=5, sticky="ew")
        logger.debug("控件创建完成。")

    def _create_window_frame(self, parent) -> ttk.Frame:
        frame = ttk.Labelframe(parent, text=i18n.get("config.window.frame.title"))
        frame.columnconfigure(0, weight=1)

        scan_frame = ttk.Frame(frame)
        scan_frame.grid(row=0, column=0, sticky="ew", padx=10, pady=5)
        scan_frame.columnconfigure(1, weight=1)

        self.window_scan_btn = ttk.Button(scan_frame, text=i18n.get("config.window.btn.scan"), command=self._refresh_window_list)
        self.window_scan_btn.grid(row=0, column=0, sticky="w")

        self.window_scan_status = ttk.Label(scan_frame, text="", font=self.FONT_NORMAL)
        self.window_scan_status.grid(row=0, column=1, sticky="w", padx=(10, 0))

        list_frame = ttk.Frame(frame)
        list_frame.grid(row=1, column=0, sticky="ew", padx=10, pady=(0, 10))
        list_frame.columnconfigure(0, weight=1)

        self.window_listbox = tk.Listbox(list_frame, height=5, activestyle="dotbox")
        self.window_listbox.grid(row=0, column=0, sticky="ew")
        self.window_listbox.bind("<<ListboxSelect>>", self._on_window_select)

        scroll = ttk.Scrollbar(list_frame, command=self.window_listbox.yview)
        scroll.grid(row=0, column=1, sticky="ns")
        self.window_listbox.config(yscrollcommand=scroll.set)

        self.window_selected_label = ttk.Label(frame, text=i18n.get("config.window.selected", "Selected: None"), font=self.FONT_NORMAL)
        self.window_selected_label.grid(row=2, column=0, sticky="w", padx=10, pady=(0, 5))

        self.window_preview_label = ttk.Label(frame, text=i18n.get("config.window.preview.unavailable", "Preview unavailable"), font=self.FONT_NORMAL, anchor="center", compound="top", justify="center")
        self.window_preview_label.grid(row=3, column=0, sticky="ew", padx=10, pady=(5, 5), ipady=18)
        frame.rowconfigure(3, weight=0)

        return frame

    def _refresh_window_list(self):
        self.window_items = []
        self.window_listbox.delete(0, "end")

        if sys.platform != "win32":
            self.window_scan_status.config(text=i18n.get("config.window.scan.not_supported"))
            return

        EnumWindows = ctypes.windll.user32.EnumWindows
        GetWindowTextLengthW = ctypes.windll.user32.GetWindowTextLengthW
        GetWindowTextW = ctypes.windll.user32.GetWindowTextW
        GetClassNameW = ctypes.windll.user32.GetClassNameW
        IsWindowVisible = ctypes.windll.user32.IsWindowVisible

        candidates = []

        @ctypes.WINFUNCTYPE(ctypes.c_bool, wintypes.HWND, wintypes.LPARAM)
        def enum_proc(hwnd, lparam):
            if not IsWindowVisible(hwnd):
                return True
            length = GetWindowTextLengthW(hwnd)
            title = ""
            if length > 0:
                buffer = ctypes.create_unicode_buffer(length + 1)
                GetWindowTextW(hwnd, buffer, length + 1)
                title = buffer.value.strip()

            class_name_buffer = ctypes.create_unicode_buffer(256)
            GetClassNameW(hwnd, class_name_buffer, 256)
            class_name = class_name_buffer.value.strip()

            low_title = title.lower()
            low_class = class_name.lower()
            
            if title:
                priority = 0
                if title == "明日方舟":
                    priority = 10
                elif "明日方舟" in title:
                    priority = 8
                elif "arknights" in low_title:
                    priority = 6
                elif "方舟" in title:
                    priority = 4
                elif "unitywndclass" in low_class or "unityhwndclass" in low_class:
                    priority = 2
                else:
                    priority = 1
                candidates.append((priority, hwnd, title, class_name))
            return True

        EnumWindows(enum_proc, 0)

        # 按优先级降序，先显示重要窗口
        candidates.sort(key=lambda x: (-x[0], x[2]))

        for priority, hwnd, title, class_name in candidates:
            display = f"{title} ({class_name}) [0x{hwnd:08X}]"
            self.window_items.append((hwnd, title, class_name))
            self.window_listbox.insert("end", display)

        if self.window_items:
            self.window_scan_status.config(text=i18n.get("config.window.scan.found", "{count} windows found").format(count=len(self.window_items)))
            self.window_listbox.selection_set(0)
            self._on_window_select(None)
        else:
            self.window_scan_status.config(text=i18n.get("config.window.scan.none", "No windows found."))
            self.window_selected_label.config(text=i18n.get("config.window.selected", "Selected: None"))
            self.selected_window_handle = None
            self.selected_window_title = None

        # After updating list and labels, adjust window size in case text wraps differently.
        self._adjust_window_size()

    def _on_window_select(self, event=None):
        selections = self.window_listbox.curselection()
        if not selections:
            return
        idx = selections[0]
        hwnd, title, class_name = self.window_items[idx]
        self.selected_window_handle = int(hwnd)
        self.selected_window_title = title
        self.selected_window_class = class_name
        self.window_selected_label.config(text=i18n.get("config.window.selected", "Selected: {title}").format(title=title))
        self._start_window_preview()
        self._adjust_window_size()

    def _start_window_preview(self):
        if self.window_preview_after_id:
            self.after_cancel(self.window_preview_after_id)
            self.window_preview_after_id = None
        self._update_window_preview()

    def _stop_window_preview(self):
        if self.window_preview_after_id:
            self.after_cancel(self.window_preview_after_id)
            self.window_preview_after_id = None

    def _update_window_preview(self):
        if self.selected_window_handle is None:
            self.window_preview_label.config(text=i18n.get("config.window.preview.unavailable", "Preview unavailable"), image="")
            return

        if WindowsWindowController is None:
            self.window_preview_label.config(text="Error: Controller not found", image="")
            return

        try:
            # 强制不使用缓存，每次都重新创建控制器并连接
            with WindowsWindowController(self.selected_window_handle) as window_controller:
                img = window_controller.capture_frame()

            if img:
                # 显式转换并确保图像数据是新鲜的
                img = img.convert("RGB") 
                display_img = img.copy()
                display_img.thumbnail((500, 240))
                # 显式更新 PhotoImage 对象以强制 Tkinter 刷新
                self._window_preview_photo = ImageTk.PhotoImage(display_img)
                self.window_preview_label.config(image=self._window_preview_photo, text="")
            else:
                raise ValueError("Captured image is empty")

        except Exception as e:
            logger.warning(f"窗口预览获取失败: {e}")
            # 失败时清空旧预览图，避免误导用户
            self.window_preview_label.config(text=i18n.get("config.window.preview.error", "Preview error"), image="")
            self._window_preview_photo = None

        # 预览内容可能发生变化导致高度/宽度变化，强制重调整窗口尺寸
        self._adjust_window_size()
        self.window_preview_after_id = self.after(1000, self._update_window_preview)

    def _create_mumu_frame(self, parent) -> ttk.Frame:
        frame = ttk.Labelframe(parent, text=i18n.get("config.mumu.frame.title"))
        frame.columnconfigure(1, weight=1)

        self.mumu_path_label = ttk.Label(frame, text=i18n.get("config.label.path"), font=self.FONT_NORMAL)
        self.mumu_path_label.grid(row=0, column=0, padx=(10, 5), pady=10, sticky="w")
        self.mumu_path_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.mumu_path_entry.grid(row=0, column=1, padx=5, pady=10, sticky="ew")
        self.mumu_browse_btn = ttk.Button(frame, text=i18n.get("config.btn.browse"), command=self._browse_mumu_path, bootstyle="secondary-outline")
        self.mumu_browse_btn.grid(row=0, column=2, padx=(5, 10), pady=10, sticky="e")

        self.mumu_instance_label = ttk.Label(frame, text=i18n.get("config.label.instance"), font=self.FONT_NORMAL)
        self.mumu_instance_label.grid(row=1, column=0, padx=(10, 5), pady=(0, 10), sticky="w")
        self.mumu_instance_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.mumu_instance_entry.grid(row=1, column=1, padx=5, pady=(0, 10), sticky="ew", columnspan=2)
        self.mumu_instance_entry.insert(0, "0")
        return frame

    def _create_ldplayer_frame(self, parent) -> ttk.Frame:
        frame = ttk.Labelframe(parent, text=i18n.get("config.ldplayer.frame.title"))
        frame.columnconfigure(1, weight=1)

        self.ld_path_label = ttk.Label(frame, text=i18n.get("config.label.path"), font=self.FONT_NORMAL)
        self.ld_path_label.grid(row=0, column=0, padx=(10, 5), pady=10, sticky="w")
        self.ldplayer_path_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.ldplayer_path_entry.grid(row=0, column=1, padx=5, pady=10, sticky="ew")
        self.ld_browse_btn = ttk.Button(frame, text=i18n.get("config.btn.browse"), command=self._browse_ldplayer_path,
                                   bootstyle="secondary-outline")
        self.ld_browse_btn.grid(row=0, column=2, padx=(5, 10), pady=10, sticky="e")

        self.ld_instance_label = ttk.Label(frame, text=i18n.get("config.label.instance"), font=self.FONT_NORMAL)
        self.ld_instance_label.grid(row=1, column=0, padx=(10, 5), pady=10, sticky="w")
        self.ldplayer_instance_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.ldplayer_instance_entry.grid(row=1, column=1, padx=5, pady=10, sticky="ew", columnspan=2)
        self.ldplayer_instance_entry.insert(0, "0")

        self.ld_adb_label = ttk.Label(frame, text=i18n.get("config.label.adb_id_optional"), font=self.FONT_NORMAL)
        self.ld_adb_label.grid(row=2, column=0, padx=(10, 5), pady=(0, 10), sticky="w")
        self.ldplayer_id_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.ldplayer_id_entry.grid(row=2, column=1, padx=5, pady=(0, 10), sticky="ew", columnspan=2)
        return frame

    def _create_minicap_frame(self, parent) -> ttk.Frame:
        frame = ttk.Labelframe(parent, text=i18n.get("config.minicap.frame.title"))
        frame.columnconfigure(0, weight=1)
        self.minicap_adb_label = ttk.Label(frame, text=i18n.get("config.label.adb_id_auto"), font=self.FONT_NORMAL)
        self.minicap_adb_label.grid(row=0, column=0, padx=10, pady=(10, 5), sticky="w")
        self.minicap_id_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.minicap_id_entry.grid(row=1, column=0, padx=10, pady=(0, 10), sticky="ew")
        return frame

    def _on_language_change(self, event=None):
        """Handle language change"""
        new_lang = self.language_var.get()
        logger.info(f"Language changed to {new_lang}")
        i18n.load_locale(new_lang)
        self._refresh_ui_text()
        
    def _refresh_ui_text(self):
        """Update strings in the UI when language changes"""
        self.title(i18n.get("config.window.title"))
        self.header_label.config(text=i18n.get("config.header"))
        
        # Emulator types
        # Note: We need to update keys to map to values in localized EMULATOR_OPTIONS or just update the dropdown values
        # Since logic relies on the display name to map back to key (mumu, etc), we must be careful.
        # Ideally, we should separate display name from logic key.
        # For now, let's just update the static texts and leave the combobox items alone or rebuild them.
        # But wait, self.EMULATOR_OPTIONS keys are display names! This is bad design in original code but I must work with it.
        # To fix this properly, I'd need to change how EMULATOR_OPTIONS works.
        # Let's try to update display names.
        
        # Re-populate EMULATOR_OPTIONS with translated keys? No, ConfigWindow is short lived.
        # Let's just update labels first.
        
        self.type_frame.config(text=i18n.get("config.emulator_type"))
        self.save_button.config(text=i18n.get("config.btn.save_start"))
        
        self.mumu_frame.config(text=i18n.get("config.mumu.frame.title"))
        self.mumu_path_label.config(text=i18n.get("config.label.path"))
        self.mumu_browse_btn.config(text=i18n.get("config.btn.browse"))
        self.mumu_instance_label.config(text=i18n.get("config.label.instance"))
        
        self.ldplayer_frame.config(text=i18n.get("config.ldplayer.frame.title"))
        self.ld_path_label.config(text=i18n.get("config.label.path"))
        self.ld_browse_btn.config(text=i18n.get("config.btn.browse"))
        self.ld_instance_label.config(text=i18n.get("config.label.instance"))
        self.ld_adb_label.config(text=i18n.get("config.label.adb_id_optional"))
        
        self.minicap_frame.config(text=i18n.get("config.minicap.frame.title"))
        self.minicap_adb_label.config(text=i18n.get("config.label.adb_id_auto"))

        self.window_frame.config(text=i18n.get("config.window.frame.title"))
        self.window_scan_btn.config(text=i18n.get("config.window.btn.scan"))
        self.window_selected_label.config(text=i18n.get("config.window.selected", "Selected: None").format(title=self.selected_window_title or "None"))
        self.window_preview_label.config(text=i18n.get("config.window.preview.unavailable", "Preview unavailable"), image="")

        # 调整窗口大小以适应新的标题文本
        self._adjust_window_size()

        # Refill emulator options with translated strings while keeping the mapping
        current_selection_key = self.EMULATOR_OPTIONS.get(self.selected_display_name.get())
        
        self.EMULATOR_OPTIONS = {
            i18n.get("config.type.mumu"): "mumu",
            i18n.get("config.type.ldplayer"): "ldplayer",
            i18n.get("config.type.minicap"): "minicap",
            i18n.get("config.type.window"): "window"
        }
        self.emulator_combobox['values'] = list(self.EMULATOR_OPTIONS.keys())
        
        # Restore selection
        for disp, key in self.EMULATOR_OPTIONS.items():
            if key == current_selection_key:
                self.emulator_combobox.set(disp)
                break

    def _on_selection_change(self, event=None):
        display_name = self.selected_display_name.get()
        selected_type = self.EMULATOR_OPTIONS.get(display_name)
        logger.debug(f"连接类型切换为: {selected_type}")

        # Show/hide panes
        self.mumu_frame.grid_remove()
        self.ldplayer_frame.grid_remove()
        self.minicap_frame.grid_remove()
        self.window_frame.grid_remove()

        if selected_type == "mumu":
            self.mumu_frame.grid()
            self._stop_window_preview()
        elif selected_type == "ldplayer":
            self.ldplayer_frame.grid()
            self._stop_window_preview()
        elif selected_type == "minicap":
            self.minicap_frame.grid()
            self._stop_window_preview()
        elif selected_type == "window":
            self.window_frame.grid()
            self._refresh_window_list()
            self._start_window_preview()
        else:
            self.minicap_frame.grid()
            self._stop_window_preview()

        self.selected_display_name.set(display_name)
        self._adjust_window_size()


    def _browse_mumu_path(self):
        logger.debug("打开 '浏览' 对话框以选择MuMu路径。")
        path = filedialog.askdirectory(title=i18n.get("config.browse.title.mumu"))
        if path:
            logger.info(f"用户选择了MuMu路径: {path}")
            self.mumu_path_entry.delete(0, "end")
            self.mumu_path_entry.insert(0, path)
        else:
            logger.debug("用户取消了路径选择。")

    def _browse_ldplayer_path(self):
        logger.debug("打开 '浏览' 对话框以选择雷电模拟器路径。")
        path = filedialog.askdirectory(title=i18n.get("config.browse.title.ldplayer"))
        if path:
            logger.info(f"用户选择了雷电模拟器路径: {path}")
            self.ldplayer_path_entry.delete(0, "end")
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
                Messagebox.show_error(i18n.get("config.error.mumu_path_empty"), title=i18n.get("config.error.mumu_path_empty.title"), parent=self)
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
                Messagebox.show_error(i18n.get("config.error.ld_path_empty"), title=i18n.get("config.error.ld_path_empty.title"), parent=self)
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

        elif cfg_type == "window":
            if self.selected_window_handle is None:
                logger.warning("保存失败：未选择窗口。")
                Messagebox.show_error(i18n.get("config.window.error.no_window_selected"), title=i18n.get("config.window.error.no_window_selected.title"), parent=self)
                return
            if self.selected_window_title:
                self.config_data["window_title"] = self.selected_window_title
            self.config_data["window_handle"] = self.selected_window_handle
        if self.selected_window_title:
            self.config_data["window_title"] = self.selected_window_title
        if self.selected_window_class:
            self.config_data["window_class"] = self.selected_window_class

        else:  # minicap
            minicap_id = self.minicap_id_entry.get().strip()
            if minicap_id:
                self.config_data["device_id"] = minicap_id

        self.config_data["language"] = self.language_var.get()

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
