# ark_timeline_tool/config.py

# --- 项目信息 ---
VERSION = "v0.4"

# --- 应用设置 ---
FPS = 30
NODE_COLORS = ["#ff6347", "#4682b4", "#32cd32", "#ffd700", "#9370db", "#ffa500"]
WINDOW_WIDTH = 1000
WINDOW_HEIGHT = WINDOW_WIDTH // 5
DEFAULT_ALPHA = 0.85
ICON_SIZE = (30, 30)

# --- 时间轴与节点（新配置 & 调整）---
NODE_FIND_TOLERANCE = 3        # 光标附近查找节点的容差（帧）
NODE_CLICK_TOLERANCE = 10      # <<< 新增：点击节点的容差范围（帧），比查找范围大更易于点击
NODE_DIAMOND_SIZE = {"h": 12, "w": 8} # <<< 修改：节点菱形的大小

# --- 交互手感（新配置 & 调整）---
TIMELINE_DRAG_SENSITIVITY = 2  # <<< 修改：时间轴拖动灵敏度，分母越小越灵敏
MAGNET_BREAK_THRESHOLD = 30    # <<< 新增：在磁铁模式下拖动超过多少像素时自动关闭磁铁模式

# --- 网络设置 ---
WEBSOCKET_URI = "ws://localhost:2606"
WEBSOCKET_RECONNECT_DELAY = 5
QUEUE_POLL_INTERVAL = 16

# --- 日志和资源目录 ---
LOG_DIR = "logs"
ICON_DIR = "icons"

