@echo off
REM AZRA - FAST ITERATION launcher (fast-release profile).
REM Small-change rebuilds take ~tens of seconds (opt2 + thin LTO + 16 CGU + incremental)
REM instead of full release's 6-8 minutes. Feel is near-release.
REM RULE: perf measurements / PERF_TARGETS numbers still come from Run_UIv2_Release.bat
REM (--release) ONLY - never quote fast-release timings.
set "UIWT=%~dp0"
if not exist "%UIWT%core_engine\Cargo.toml" (
  echo ERROR: no core_engine\Cargo.toml under %UIWT%
  pause
  exit /b 1
)
cd /d "%UIWT%core_engine"
echo [AZRA] FAST iteration build - worktree: %UIWT%
for /f %%B in ('git -C "%UIWT%" rev-parse --abbrev-ref HEAD 2^>nul') do set "CUR=%%B"
echo [AZRA] branch: %CUR%
where cmake >nul 2>nul || set "PATH=C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;%PATH%"
set AZRA_UI=v2
REM Dev capture: every free-knife subtract writes the exact pre-cut mesh + cutter to
REM %TEMP%\azra_knife_repro.json (bug-repro rig for the "knife breaks geo" defect).
set AZRA_KNIFE_DUMP=1
echo [AZRA] building (fast-release, incremental)...
cargo build -p app --profile fast-release
if errorlevel 1 (
  echo [AZRA] BUILD FAILED - see errors above.
  pause
  exit /b 1
)
echo [AZRA] launching...
"%UIWT%core_engine\target\fast-release\app.exe"
echo.
echo [AZRA] app exited.
pause
