@echo off
cd /d "%~dp0"
set PATH=%PATH%;%USERPROFILE%\.cargo\bin
title layer
pnpm tauri dev
