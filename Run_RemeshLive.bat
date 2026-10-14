@echo off
REM Voxel Remesh live-incr worktree launcher (fast-release).
REM Branch: feat/remesh-live-incr
REM Measure remesh timings with --release cargo tests, not this profile.
set "UIWT=%~dp0"
if not exist "%UIWT%core_engine\Cargo.toml" (
  echo ERROR: no core_engine\Cargo.toml under %UIWT%
  pause
  exit /b 1
)
cd /d "%UIWT%core_engine"
echo [AZRA] REMESH LIVE worktree: %UIWT%
for /f %%B in ('git -C "%UIWT%" rev-parse --abbrev-ref HEAD 2^>nul') do set "CUR=%%B"
echo [AZRA] branch: %CUR%
where cmake >nul 2>nul || set "PATH=C:\Program Files\Microsoft Visual Studio\18\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;%PATH%"
set AZRA_UI=v2
set MANIFOLD_CSG_LIB_DIR=F:\PROJECT\FourthLAyerAi\dev\manifold-prebuilt
set MANIFOLD_CSG_LIB_KIND=static
echo [AZRA] building (fast-release)...
cargo build -p app --profile fast-release
if errorlevel 1 (
  echo [AZRA] BUILD FAILED - see errors above.
  pause
  exit /b 1
)
echo [AZRA] launching remesh-live-incr...
echo.
echo  Test: add Voxel Remesh to a poly body, grab a cage vertex, the remeshed
echo  surface must update live under the drag. Resolution 128 is the live
echo  scrub range; 256 applies on slider release but a vertex drag stays live.
echo.
"%UIWT%core_engine\target\fast-release\app.exe"
echo.
echo [AZRA] app exited.
pause
