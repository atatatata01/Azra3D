@echo off
REM ============================================================
REM  FourthLayer / Azra — AI Texture + Neural Beauty server
REM  Keep this window open while using AI Texture / AI Beauty.
REM ============================================================
cd /d "%~dp0ai_backend\server"

set PYTHONIOENCODING=utf-8

REM Prefer user override, then common installs, then PATH
if defined AZRA_AI_PYTHON (
  set "PY=%AZRA_AI_PYTHON%"
  goto :run
)
if exist "%LOCALAPPDATA%\Programs\Python\Python311\python.exe" (
  set "PY=%LOCALAPPDATA%\Programs\Python\Python311\python.exe"
  goto :run
)
if exist "%LOCALAPPDATA%\Programs\Python\Python312\python.exe" (
  set "PY=%LOCALAPPDATA%\Programs\Python\Python312\python.exe"
  goto :run
)
if exist "%USERPROFILE%\miniconda3\python.exe" (
  set "PY=%USERPROFILE%\miniconda3\python.exe"
  goto :run
)
set "PY=python"

:run
echo.
echo  Using: %PY%
echo  Azra AI Pipeline: %CD%\texture_server.py
echo  Model:  Flux.1 Kontext (local ComfyUI weights)
echo  Bind:   http://127.0.0.1:7860
echo  Beauty: POST /neural_beauty  (Kontext + structure lock)
echo.
"%PY%" -c "import torch; print('torch', torch.__version__, 'cuda', torch.cuda.is_available())" 2>nul
if errorlevel 1 (
  echo [!] torch not found in this Python — Neural Beauty will use classical polish only.
  echo     Or set AZRA_AI_PYTHON to a Python that has torch.
  echo.
)

"%PY%" texture_server.py --model flux-kontext --port 7860
echo.
echo Server exited. Press any key to close.
pause >nul
