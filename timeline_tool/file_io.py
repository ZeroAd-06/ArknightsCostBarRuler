import json
import logging
from tkinter import filedialog, messagebox

logger = logging.getLogger(__name__)

def load_timeline_from_file(parent_widget):
    """
    打开文件对话框以加载时间轴JSON文件。
    返回加载的数据或None。
    """
    filepath = filedialog.askopenfilename(
        title="打开时间轴文件",
        filetypes=[("JSON files", "*.json"), ("All files", "*.*")],
        parent=parent_widget
    )
    if not filepath:
        return None
    try:
        with open(filepath, 'r', encoding='utf-8') as f:
            data = json.load(f)
        logger.info(f"成功加载时间轴: {filepath}")
        return data
    except Exception as e:
        logger.error(f"加载文件失败: {filepath}，错误: {e}")
        messagebox.showerror("加载失败", f"无法加载或解析文件: \n{e}", parent=parent_widget)
        return None

def save_timeline_to_file(data, parent_widget):
    """
    打开文件对话框以保存时间轴数据到JSON文件。
    """
    filepath = filedialog.asksaveasfilename(
        title="保存时间轴文件",
        defaultextension=".json",
        filetypes=[("JSON files", "*.json"), ("All files", "*.*")],
        parent=parent_widget
    )
    if not filepath:
        return
    try:
        with open(filepath, 'w', encoding='utf-8') as f:
            json.dump(data, f, indent=4, ensure_ascii=False)
        logger.info(f"成功保存时间轴: {filepath}")
    except Exception as e:
        logger.error(f"保存文件失败: {filepath}，错误: {e}")
        messagebox.showerror("保存失败", f"无法写入文件: \n{e}", parent=parent_widget)
