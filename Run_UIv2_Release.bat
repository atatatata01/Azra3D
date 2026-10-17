@echo off
setlocal
REM ---------------------------------------------------------------------------
REM  AZRA - build (first run only) and launch the application.
REM  Release build, newer interface (AZRA_UI=v2).
REM  Prerequisites and troubleshooting: see README.md ("Installation").
REM ---------------------------------------------------------------------------

cd /d "%~dp0core_engine" 2>nul
if errorlevel 1 (
  echo [AZRA] ERROR: the core_engine folder was not found next to this script.
  pause
  exit /b 1
)

where cargo >nul 2>nul
if errorlevel 1 (
  echo [AZRA] ERROR: Rust ^(cargo^) was not found. Install it from https://rustup.rs
  echo        then open a new terminal. See README.md, section "Installation".
  pause
  exit /b 1
)

REM CMake is needed to build one dependency. Visual Studio ships a copy that is
REM usually not on PATH: look for it if cmake is not already available.
where cmake >nul 2>nul
if errorlevel 1 (
  set "VSROOT=%ProgramFiles%\Microsoft Visual Studio"
  for %%V in (2022 18 2019) do for %%E in (Community Professional Enterprise BuildTools) do (
    if exist "%ProgramFiles%\Microsoft Visual Studio\%%V\%%E\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe" (
      set "PATH=%ProgramFiles%\Microsoft Visual Studio\%%V\%%E\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;%PATH%"
    )
  )
)
where cmake >nul 2>nul
if errorlevel 1 (
  echo [AZRA] ERROR: CMake was not found. Install it from https://cmake.org/download/
  echo        ^(tick "Add CMake to the system PATH"^). See README.md, section "Installation".
  pause
  exit /b 1
)

echo [AZRA] Building and starting AZRA ^(the first build can take a long time^)...
set AZRA_UI=v2
cargo run -p app --release
echo.
echo [AZRA] AZRA has exited.
pause
