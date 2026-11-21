@echo off
rem openOrchestra on Windows: `.\make <target>` from PowerShell or cmd, no
rem make needed. The targets live in script\windows\make.ps1 (`.\make help`).
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0script\windows\make.ps1" %*
