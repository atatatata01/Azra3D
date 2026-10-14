@echo off
setlocal
set "ROOT=F:\PROJECT\FourthLAyerAi"
set "NAME=SPINE"
title AZRA - %NAME%

:: 3D Spine / Pipe (SDF Path Object) — builds main repo after merge to master.
:: Plan: docs/planning/sdf/SPINE_3D_PIPE_PLAN.md
::
::   Dev_Spine            build + run
::   Dev_Spine code       open repo in VS Code
::   Dev_Spine cd         open a terminal at repo root
::   Dev_Spine build      build only (no launch)
if /I "%~1"=="code"  ( echo Opening %NAME% in VS Code... & code "%ROOT%" & goto :eof )
if /I "%~1"=="cd"    ( cd /d "%ROOT%" & echo %NAME% & cmd /k & goto :eof )
if /I "%~1"=="build" ( set "RUNAPP=0" ) else ( set "RUNAPP=1" )

if not exist "%ROOT%\core_engine\Cargo.toml" (
  echo.
  echo ERROR: repo missing or incomplete:
  echo   %ROOT%
  echo.
  pause
  goto :eof
)

cd /d "%ROOT%\core_engine"
echo ============================================================
echo   %NAME%   -- SDF 3D Spine / Pipe
echo   %ROOT%
echo ============================================================
echo Building (cargo build -p app)...
cargo build -p app
if not %ERRORLEVEL%==0 ( echo. & echo BUILD FAILED. & pause & goto :eof )
if "%RUNAPP%"=="1" ( echo Launching... & start "" "target\debug\app.exe" )
echo Done.
pause
