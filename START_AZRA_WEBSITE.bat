@echo off
setlocal
cd /d "%~dp0website"
echo AZRA website preview
echo.
echo Open this URL in your browser:
echo http://localhost:8080/
echo.
echo Keep this window open while previewing the site.
echo Press Ctrl+C to stop the server.
echo.
py -m http.server 8080 --bind localhost
if errorlevel 1 (
  echo.
  echo Python launcher failed. Trying python directly...
  python -m http.server 8080 --bind localhost
)
