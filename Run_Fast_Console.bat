@echo off
REM AZRA FAST launch — visible console + tee to %TEMP%\azra_app_console.log
set "UIWT=%~dp0"
if not exist "%UIWT%core_engine\Cargo.toml" (
  echo ERROR: no core_engine\Cargo.toml under %UIWT%
  pause
  exit /b 1
)
cd /d "%UIWT%core_engine"
where cmake >nul 2>nul || set "PATH=C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;%PATH%"
set AZRA_UI=v2
set AZRA_KNIFE_DUMP=1
set "LOG=%TEMP%\azra_app_console.log"
echo [AZRA] FAST + console log
echo [AZRA] log: %LOG%
echo [AZRA] building (fast-release)...
cargo build -p app --profile fast-release
if errorlevel 1 (
  echo [AZRA] BUILD FAILED
  pause
  exit /b 1
)
echo.>>"%LOG%"
echo === APP START %DATE% %TIME% ===>>"%LOG%"
echo [AZRA] launching (stdout+stderr appended to log; also printed here)...
"%UIWT%core_engine\target\fast-release\app.exe" 1>>"%LOG%" 2>&1
echo === APP EXIT %DATE% %TIME%  exit=%ERRORLEVEL% ===>>"%LOG%"
echo [AZRA] app exited. Full log: %LOG%
pause
