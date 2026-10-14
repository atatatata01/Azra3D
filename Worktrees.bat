@echo off
setlocal
set "ROOT=F:\PROJECT\FourthLAyerAi"
set "WT=%ROOT%\.claude\worktrees"
cd /d "%ROOT%"

:menu
cls
echo ============================================================
echo                     AZRA WORKTREES
echo ============================================================
git worktree list
echo ------------------------------------------------------------
echo   Ahead of master (uncollected work):
for %%B in (feat/ui-polish feat/poly-parametric-prims fix/ui-floor-perf feat/viewport-perf) do (
  for /f %%A in ('git rev-list --count master..%%B 2^>nul') do echo       +%%A   %%B
)
echo ------------------------------------------------------------
echo   master ^(repo ROOT^) = RELEASE TRUNK. Branch off it, merge back.
echo   feat/ui-polish is a DEPRECATED name kept so its worktree works;
echo   ui2 / theme / dock work now lands on master. See docs\WORKTREES.md
echo ------------------------------------------------------------
echo   Open in VS Code:
echo       1. Master (release trunk)   2. UI polish
echo       3. Poly parametric prims    4. UI floor perf
echo       5. Viewport perf            Q. Quit
echo.
echo   Launchers:
echo       Dev_Azra.bat        = master product UI
echo       Run_Fast.bat        = fast-release build of THIS checkout
echo       Run_UIv2.bat        = UI v2 shell (ui-polish worktree, AZRA_UI=v2)
echo       Dev_UI.bat          = same worktree build/run helpers
echo ============================================================
set "S="
set /p S=Open in VS Code [1-5, Q]:
if /I "%S%"=="Q" goto :eof
if "%S%"=="1" ( start "" code "%ROOT%" & goto menu )
if "%S%"=="2" ( call :open ui-polish            & goto menu )
if "%S%"=="3" ( call :open poly-params          & goto menu )
if "%S%"=="4" ( call :open ui-floor-fix         & goto menu )
if "%S%"=="5" ( call :open viewport-perf        & goto menu )
goto menu

:open
if not exist "%WT%\%~1\CLAUDE.md" (
  echo ERROR: worktree "%~1" missing. Recreate it - see docs\WORKTREES.md
  pause
) else (
  start "" code "%WT%\%~1"
)
goto :eof
