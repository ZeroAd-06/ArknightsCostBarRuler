import json
import tkinter as tk
from tkinter import ttk
from tkinter import filedialog, messagebox
from typing import Dict, Any, Optional

CONFIG_FILE = "config.json"


def load_config() -> Optional[Dict[str, Any]]:
    """从 config.json 加载配置。如果文件不存在或为空，返回 None。"""
    try:
        with open(CONFIG_FILE, 'r', encoding='utf-8') as f:
            config = json.load(f)
            if config:
                return config
            return None
    except (FileNotFoundError, json.JSONDecodeError):
        return None


def save_config(config: Dict[str, Any]):
    """将配置字典保存到 config.json。"""
    with open(CONFIG_FILE, 'w', encoding='utf-8') as f:
        json.dump(config, f, indent=4, ensure_ascii=False)


class ConfigWindow(tk.Tk):
    """
    一个Tkinter窗口，用于引导用户完成首次配置。
    """

    def __init__(self):
        super().__init__()
        self.config_data = None
        self.style = ttk.Style(self)
        self.style.theme_use('clam')
        self.FONT_NORMAL = ("Microsoft YaHei UI", 10)
        self.FONT_BOLD = ("Microsoft YaHei UI", 11, "bold")
        self.title("首次使用配置向导")
        self.config(bg="#f0f0f0")
        main_frame = ttk.Frame(self, padding="15 15 15 15")
        main_frame.pack(expand=True, fill=tk.BOTH)
        self.controller_type = tk.StringVar(master=self, value="mumu")
        self._create_widgets(main_frame)
        self._on_radio_change()
        self.update_idletasks()
        self._center_window()
        self.resizable(False, False)

    def _center_window(self):
        """将窗口置于屏幕中央。"""
        self.update_idletasks()
        width = self.winfo_width()
        height = self.winfo_height()
        screen_width = self.winfo_screenwidth()
        screen_height = self.winfo_screenheight()
        x = (screen_width // 2) - (width // 2)
        y = (screen_height // 2) - (height // 2)
        self.geometry(f'{width}x{height}+{x}+{y}')

    def _create_widgets(self, parent: ttk.Frame):
        """创建所有UI组件并使用grid布局。"""
        parent.columnconfigure(0, weight=1)
        header_label = ttk.Label(
            parent,
            text="首次使用，请完成连接配置。",
            font=("Microsoft YaHei UI", 14, "bold"),
            foreground="#333"
        )
        header_label.grid(row=0, column=0, pady=(0, 20), sticky="w")
        type_frame = ttk.Labelframe(parent, text=" 模拟器类型 ", labelanchor="nw", style='TLabelframe')
        type_frame.grid(row=1, column=0, sticky="ew", pady=10)
        type_frame.columnconfigure(0, weight=1)
        type_frame.columnconfigure(1, weight=1)
        ttk.Radiobutton(type_frame, text="MuMu模拟器12", variable=self.controller_type, value="mumu",
                        command=self._on_radio_change).grid(row=0, column=0, padx=10, pady=5, sticky="w")
        ttk.Radiobutton(type_frame, text="其他", variable=self.controller_type, value="minicap",
                        command=self._on_radio_change).grid(row=0, column=1, padx=10, pady=5, sticky="w")
        self.options_container = ttk.Frame(parent)
        self.options_container.grid(row=2, column=0, sticky="ew", pady=5)
        self.options_container.columnconfigure(0, weight=1)
        self.mumu_frame = self._create_mumu_frame(self.options_container)
        self.mumu_frame.grid(row=0, column=0, sticky="nsew")
        self.minicap_frame = self._create_minicap_frame(self.options_container)
        self.minicap_frame.grid(row=0, column=0, sticky="nsew")
        self.style.configure('Success.TButton', font=self.FONT_BOLD, foreground='white', background='#28a745')
        self.style.map('Success.TButton', background=[('active', '#218838')])
        save_button = ttk.Button(
            parent,
            text="保存并启动",
            command=self._save_and_close,
            style='Success.TButton'
        )
        save_button.grid(row=3, column=0, pady=(20, 0), ipady=5, sticky="ew")

    def _create_mumu_frame(self, parent) -> ttk.Frame:
        """创建MuMu模拟器的配置界面。"""
        frame = ttk.Labelframe(parent, text=" MuMu模拟器12 设置 ", labelanchor="nw")
        frame.columnconfigure(1, weight=1)
        label = ttk.Label(frame, text="安装路径:", font=self.FONT_NORMAL)
        label.grid(row=0, column=0, padx=(10, 5), pady=10, sticky="w")
        self.mumu_path_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.mumu_path_entry.grid(row=0, column=1, padx=5, pady=10, sticky="ew")
        browse_button = ttk.Button(frame, text="浏览...", command=self._browse_mumu_path)
        browse_button.grid(row=0, column=2, padx=(5, 10), pady=10, sticky="e")
        return frame

    def _create_minicap_frame(self, parent) -> ttk.Frame:
        """创建ADB/Minicap的配置界面。"""
        frame = ttk.Labelframe(parent, text=" ADB 设置 ", labelanchor="nw")
        frame.columnconfigure(0, weight=1)
        label = ttk.Label(frame, text="ADB Device ID (可选, 留空则自动检测):", font=self.FONT_NORMAL)
        label.grid(row=0, column=0, padx=10, pady=(10, 5), sticky="w")
        self.minicap_id_entry = ttk.Entry(frame, font=self.FONT_NORMAL)
        self.minicap_id_entry.grid(row=1, column=0, padx=10, pady=(0, 10), sticky="ew")
        return frame

    def _on_radio_change(self):
        """当单选按钮变化时，提升对应的Frame到顶层。"""
        if self.controller_type.get() == "mumu":
            self.mumu_frame.tkraise()
        else:
            self.minicap_frame.tkraise()

    def _browse_mumu_path(self):
        """浏览并选择MuMu模拟器的安装根目录。"""
        path = filedialog.askdirectory(title="请选择MuMu模拟器12的安装根目录")
        if path:
            self.mumu_path_entry.delete(0, tk.END)
            self.mumu_path_entry.insert(0, path)

    def _save_and_close(self):
        """验证输入、保存配置并关闭窗口。"""
        cfg_type = self.controller_type.get()
        self.config_data = {"type": cfg_type, "active_calibration_profile": None} # 新增

        if cfg_type == "mumu":
            mumu_path = self.mumu_path_entry.get().strip()
            if not mumu_path:
                messagebox.showerror("错误", "MuMu模拟器安装路径不能为空！")
                return
            self.config_data["install_path"] = mumu_path
        else:  # minicap
            minicap_id = self.minicap_id_entry.get().strip()
            if minicap_id:
                self.config_data["device_id"] = minicap_id

        save_config(self.config_data)
        self.destroy()


def create_config_with_gui() -> Optional[Dict[str, Any]]:
    """启动GUI让用户创建配置。如果用户中途关闭窗口，返回None。"""
    window = ConfigWindow()
    window.mainloop()
    return window.config_data


if __name__ == "__main__":
    existing_config = load_config()
    if existing_config:
        print("配置已存在:", existing_config)
    else:
        print("未找到配置文件，启动配置向导...")
        new_config = create_config_with_gui()
        if new_config:
            print("新配置已创建:", new_config)
        else:
            print("用户关闭了配置向导，未创建配置。")