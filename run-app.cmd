@echo off
rem AgentCurator 桌面端启动器
rem 始终指向 target\release 下的最新构建，避免拷来拷去产生过期副本。
cd /d "%~dp0"
if not exist "target\release\curator-desktop.exe" (
  echo 找不到 target\release\curator-desktop.exe
  echo 请先执行: pnpm tauri build
  pause
  exit /b 1
)
start "" "target\release\curator-desktop.exe"
