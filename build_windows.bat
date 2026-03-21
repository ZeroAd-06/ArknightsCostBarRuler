@echo off
chcp 65001 > nul
setlocal enabledelayedexpansion

set ROOT=%~dp0
set ROOT=%ROOT:~0,-1%
cd /d "%ROOT%"

set VENV_DIR=.venv_build

if exist "dist" rd /s /q "dist"
if exist "build" rd /s /q "build"
if exist "%VENV_DIR%" rd /s /q "%VENV_DIR%"

python -m venv "%VENV_DIR%"
if !ERRORLEVEL! neq 0 exit /b !ERRORLEVEL!

set "PY_EXE=%VENV_DIR%\Scripts\python.exe"
set "PIP_EXE=%VENV_DIR%\Scripts\pip.exe"

call "!PIP_EXE!" install -r "requirements.txt"
call "!PIP_EXE!" install pyinstaller

call "!PY_EXE!" -m PyInstaller --clean --noconfirm --onedir --windowed --uac-admin ^
    --name ArknightsCostBarRuler ^
    --distpath "dist" ^
    --workpath "build\ArknightsCostBarRuler" ^
    --paths "." ^
    --add-data "ruler\locales;ruler\locales" ^
    --add-data "ruler\controllers\minicap;ruler\controllers\minicap" ^
    --add-data "icons;icons" ^
    --hidden-import "controllers.windows" ^
    --hidden-import "controllers.mumu" ^
    --hidden-import "controllers.ldplayer" ^
    --hidden-import "controllers.minicap" ^
    --exclude-module "numpy" --exclude-module "matplotlib" --exclude-module "scipy" ^
    --exclude-module "pandas" --exclude-module "torch" --exclude-module "tensorflow" ^
    "ruler\main.py"

call "!PY_EXE!" -m PyInstaller --clean --noconfirm --onedir --windowed --uac-admin ^
    --name ArknightsTimelineTool ^
    --distpath "dist" ^
    --workpath "build\ArknightsTimelineTool" ^
    --paths "." ^
    --add-data "timeline_tool\locales;timeline_tool\locales" ^
    --add-data "icons;icons" ^
    --hidden-import "utils" ^
    --exclude-module "numpy" --exclude-module "matplotlib" --exclude-module "scipy" ^
    --exclude-module "pandas" --exclude-module "torch" --exclude-module "tensorflow" ^
    "timeline_tool\main.py"

if exist "%VENV_DIR%" rd /s /q "%VENV_DIR%"
if exist "build" rd /s /q "build"

echo Build Complete.
dir "%ROOT%\dist"
pause
