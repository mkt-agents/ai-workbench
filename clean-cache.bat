@echo off
chcp 65001 >nul
echo 清理 Rust 编译缓存...
cd /d D:\ai_project\ai-workbench\src-tauri
cargo clean
echo.
echo 清理完成！
echo 按任意键关闭...
pause >nul