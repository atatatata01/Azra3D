@echo off
REM AZRA - launch FINAL UI (v2) in RELEASE (owner feel-tests / perf gates).
REM Dev iteration stays on Run_UIv2.bat (debug profile).
REM RULE: UI v2 lives ONLY on feat/ui-polish (ui-polish worktree) when that is the
REM product UI branch. This worktree may override UIWT for local material-render work.
set "UIWT=%~dp0"
if not exist "%UIWT%core_engine\Cargo.toml" (
  set "UIWT=F:\PROJECT\FourthLAyerAi\.claude\worktrees\ui-polish"
)
if not exist "%UIWT%\core_engine\Cargo.toml" (
  echo ERROR: no core_engine\Cargo.toml under %UIWT%
  pause
  exit /b 1
)
cd /d "%UIWT%\core_engine"
echo [AZRA] Final UI (v2) RELEASE - worktree: %UIWT%
for /f %%B in ('git -C "%UIWT%" rev-parse --abbrev-ref HEAD 2^>nul') do set "CUR=%%B"
echo [AZRA] branch: %CUR%
echo [AZRA] launching UI v2 release (AZRA_UI=v2 --release)...
where cmake >nul 2>nul || set "PATH=C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;%PATH%"
set AZRA_UI=v2
cargo run -p app --release
echo.
echo [AZRA] app exited.
pause
