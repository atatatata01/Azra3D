@echo off
setlocal
set "WT=F:\PROJECT\FourthLAyerAi\.claude\worktrees\ui-polish"
set "NAME=UI-POLISH"
set "BRANCH=feat/ui-polish"
title AZRA - %NAME%

:: UI v2 / polish worktree ONLY. Never build UI redesign from master.
::   Dev_UI            build + run with AZRA_UI=v2
::   Dev_UI code       open this worktree in VS Code
::   Dev_UI cd         open a terminal in this worktree
::   Dev_UI build      build only (no launch)
:: Prefer Run_UIv2.bat for a one-click v2 launch from the main dir.
if /I "%~1"=="code"  ( echo Opening %NAME% in VS Code... & code "%WT%" & goto :eof )
if /I "%~1"=="cd"    ( cd /d "%WT%" & echo %NAME% [%BRANCH%] & cmd /k & goto :eof )
if /I "%~1"=="build" ( set "RUNAPP=0" ) else ( set "RUNAPP=1" )

if not exist "%WT%\core_engine\Cargo.toml" (
  echo.
  echo ERROR: UI worktree missing or incomplete:
  echo   %WT%
  echo.
  echo Recreate with:
  echo   cd /d F:\PROJECT\FourthLAyerAi
  echo   git worktree add .claude\worktrees\ui-polish feat/ui-polish
  echo.
  pause
  goto :eof
)

cd /d "%WT%"
for /f %%B in ('git rev-parse --abbrev-ref HEAD') do set "CUR=%%B"
if /I not "%CUR%"=="%BRANCH%" (
  echo.
  echo ERROR: worktree is on [%CUR%], expected [%BRANCH%].
  echo Refuse to run UI tooling on the wrong branch.
  echo.
  pause
  goto :eof
)

cd /d "%WT%\core_engine"
echo ============================================================
echo   %NAME%   [%BRANCH%]   -- UI v2 / polish  (NOT master)
echo   %WT%
echo   AZRA_UI=v2
echo ============================================================
echo Building (cargo build -p app)...
cargo build -p app
if not %ERRORLEVEL%==0 ( echo. & echo BUILD FAILED. & pause & goto :eof )
if "%RUNAPP%"=="1" (
  echo Launching with AZRA_UI=v2...
  set AZRA_UI=v2
  start "" "target\debug\app.exe"
)
echo Done.
pause
