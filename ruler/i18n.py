import json
import logging
import os
import locale
import sys
from typing import Dict, Optional

logger = logging.getLogger(__name__)

class I18n:
    _instance = None
    
    def __new__(cls):
        if cls._instance is None:
            cls._instance = super(I18n, cls).__new__(cls)
            cls._instance._initialized = False
        return cls._instance

    def _get_locale_candidates(self):
        candidates = []
        module_dir = os.path.dirname(os.path.abspath(__file__))

        # Standard project layout
        candidates.append(os.path.join(module_dir, 'locales'))
        candidates.append(os.path.join(module_dir, '..', 'locales'))
        candidates.append(os.path.join(module_dir, '..', '..', 'locales'))
        candidates.append(os.path.join(module_dir, 'ruler', 'locales'))

        # PyInstaller onefile or onedir extraction path
        if hasattr(sys, '_MEIPASS') and sys._MEIPASS:
            candidates.append(os.path.join(sys._MEIPASS, 'locales'))
            candidates.append(os.path.join(sys._MEIPASS, 'ruler', 'locales'))
            candidates.append(os.path.join(sys._MEIPASS, '_internal', 'locales'))
            candidates.append(os.path.join(sys._MEIPASS, '_internal', 'ruler', 'locales'))

        # Relative to executable path
        executable_dir = os.path.dirname(os.path.abspath(sys.executable))
        candidates.append(os.path.join(executable_dir, 'locales'))
        candidates.append(os.path.join(executable_dir, 'ruler', 'locales'))
        candidates.append(os.path.join(executable_dir, '_internal', 'locales'))
        candidates.append(os.path.join(executable_dir, '_internal', 'ruler', 'locales'))

        # Current working directory
        cwd = os.path.abspath(os.getcwd())
        candidates.append(os.path.join(cwd, 'locales'))
        candidates.append(os.path.join(cwd, 'ruler', 'locales'))
        candidates.append(os.path.join(cwd, '_internal', 'locales'))
        candidates.append(os.path.join(cwd, '_internal', 'ruler', 'locales'))

        # Remove duplicates while preserving order
        seen = set()
        unique = []
        for candidate in candidates:
            normalized = os.path.normpath(candidate)
            if normalized not in seen:
                seen.add(normalized)
                unique.append(normalized)
        return unique

    def _resolve_locale_dir(self):
        candidates = self._get_locale_candidates()
        for candidate in candidates:
            if os.path.isdir(candidate):
                # Prefer directories with at least one locale file
                if any(name.endswith('.json') for name in os.listdir(candidate)):
                    logger.debug(f"Resolved locale directory: {candidate}")
                    return candidate
        # Fallback to module-local path
        fallback = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'locales')
        logger.warning(f"No locale directory found from candidates. Falling back to {fallback}")
        return fallback

    def __init__(self):
        if self._initialized:
            return

        self.locale_dir = self._resolve_locale_dir()
        self.current_locale: str = 'zh_CN'
        self.translations: Dict[str, str] = {}
        self._initialized = True
        logger.info("Ruler I18n module initialized.")

    def load_locale(self, locale_code: str):
        """加载指定的语言文件"""
        self.current_locale = locale_code
        locale_file_name = f"{locale_code}.json"

        # Re-resolve locale dir each time in case working directory changes
        self.locale_dir = self._resolve_locale_dir()
        file_path = os.path.join(self.locale_dir, locale_file_name)

        logger.info(f"Loading locale file: {file_path}")
        if not os.path.exists(file_path):
            # Try to find in candidate locations
            found = False
            for candidate in self._get_locale_candidates():
                candidate_path = os.path.join(candidate, locale_file_name)
                if os.path.isfile(candidate_path):
                    file_path = candidate_path
                    found = True
                    logger.info(f"Found locale file at fallback path: {file_path}")
                    break
            if not found:
                logger.error(f"Locale file not found: {file_path}. Falling back to empty translations.")
                self.translations = {}
                return

        try:
            with open(file_path, 'r', encoding='utf-8') as f:
                self.translations = json.load(f)
            logger.info(f"Locale '{locale_code}' loaded successfully from {file_path}.")
        except json.JSONDecodeError:
            logger.error(f"Locale file is invalid JSON: {file_path}. Falling back to empty translations.")
            self.translations = {}
        except Exception as e:
            logger.exception(f"Error loading locale '{locale_code}' from {file_path}: {e}")
            self.translations = {}

    def get(self, key: str, default: Optional[str] = None, **kwargs) -> str:
        """
        获取翻译字符串。
        :param key: 键名
        :param default: 默认值（如果为None，则返回key）
        :param kwargs: 格式化参数
        :return: 翻译后的字符串
        """
        val = self.translations.get(key, default if default is not None else key)
        if kwargs:
            try:
                return val.format(**kwargs)
            except KeyError as e:
                logger.warning(f"Missing format key '{e}' in translation for '{key}': {val}")
                return val
        return val

    def auto_detect_language(self):
        """尝试自动检测系统语言"""
        try:
            sys_lang, _ = locale.getdefaultlocale()
            logger.info(f"Detected system language: {sys_lang}")
            if sys_lang and sys_lang.startswith('en'):
                return 'en_US'
            elif sys_lang and (sys_lang.startswith('zh') or sys_lang == 'Chinese'):
                 # Covers zh_CN, zh_TW, zh_HK, etc. defaulting to zh_CN for now as we only have that.
                return 'zh_CN'
        except Exception:
            pass
        return 'zh_CN' # Default fallback

i18n = I18n()
