rem 用于在发行版中以图像转储调试模式运行程序

@echo off
rem 设置窗口标题，方便识别
title 明日方舟费用条尺子 - 调试模式

rem 更改命令行编码为 UTF-8
chcp 65001 > nul

rem 清空屏幕并显示警告信息
cls
echo.
echo ===============================================================================
echo.
echo                       !!! 警告: 调试模式 !!!
echo.
echo ===============================================================================
echo.
echo  您即将以【图像转储调试模式】启动“明日方舟费用条尺子”。
echo.
echo  此模式会记录详细的日志，并【每秒保存一张截图】到 "logs\img_dumps" 文件夹。
echo.
echo  这会导致【大量占用硬盘空间】并可能【轻微影响程序性能】。
echo.
echo  本模式不适合日常使用，仅建议在遇到程序错误、需要向开发者报告问题时使用。
echo.
echo ===============================================================================
echo.

rem 提供选择，让用户可以取消操作
choice /C YN /M "您确定要继续吗"

rem 检查用户的选择
rem 如果选择 N (第2个选项)，则跳转到 :cancel 部分
if %ERRORLEVEL% == 2 goto :cancel
rem 如果选择 Y (第1个选项)，则继续执行
if %ERRORLEVEL% == 1 goto :start


:start
rem 清屏，准备启动程序
cls
echo 正在以调试模式启动 ArknightsCostBarRuler.exe ...
echo.
echo 日志和截图将被保存在 "logs" 文件夹中。
echo.
echo 关闭程序后，此窗口将保持打开状态以便查看最终信息。
echo.

rem 使用 start 命令启动程序
rem 添加 --debug-img 参数以启用图像转储
start "" "ArknightsCostBarRuler.exe" --debug-img

goto :end


:cancel
rem 用户选择取消

goto :end


:end

