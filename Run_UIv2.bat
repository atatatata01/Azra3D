@echo off
REM AZRA - launch FINAL UI (v2).
REM NOTE: the "UI only on feat/ui-polish" gate was RETIRED 2026-08-08 - ui2 work
REM now lands on master like anything else. This launcher still builds the
REM ui-polish worktree, which tracks master. See CLAUDE.md "Branch Roles".
REM Master product UI = Dev_Azra.bat. Helpers = Dev_UI.bat
set "UIWT=F:\PROJECT\FourthLAyerAi\.claude\worktrees\ui-polish"
if not exist "%UIWT%\core_engine\Cargo.toml" (
  echo ERROR: ui-polish worktree missing: %UIWT%
  echo Recreate: git worktree add .claude\worktrees\ui-polish feat/ui-polish
  pause
  exit /b 1
)
for /f %%B in ('git -C "%UIWT%" rev-parse --abbrev-ref HEAD') do set "CUR=%%B"
if /I not "%CUR%"=="feat/ui-polish" (
  echo ERROR: worktree branch is [%CUR%], expected feat/ui-polish. Aborting.
  pause
  exit /b 1
)
cd /d "%UIWT%\core_engine"
echo [AZRA] Final UI (v2) - worktree: %UIWT%
echo [AZRA] branch: %CUR%
echo [AZRA] launching UI v2 (AZRA_UI=v2)...
where cmake >nul 2>nul || set "PATH=C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;%PATH%"
set AZRA_UI=v2
cargo run -p app
echo.
echo [AZRA] app exited.
pause
