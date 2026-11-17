@echo off
rem Starts arro.exe and finds the JDK for jdtls, jdtls and java-debug without fixed
rem paths. jdtls 1.63 needs Java 25 or newer; JAVA_HOME is the project JDK and stays as is.
rem --print-paths prints the paths that were found and exits without a window.
setlocal EnableExtensions EnableDelayedExpansion
set "BASE=%~dp0"
rem The oldest Java that runs the jdtls release Arro is tested with.
set "JDTLS_JAVA=25"
set "PRINT="
if /i "%~1"=="--print-paths" set "PRINT=1"

rem An explicit choice wins even when it is older than 25, but the user hears about it.
set "JAVA=%IDE_JAVA_HOME%"
if defined JAVA (
  call :version_of "%JAVA%"
  if defined MAJOR_KNOWN if !MAJOR! LSS %JDTLS_JAVA% echo run-arro: IDE_JAVA_HOME is Java !MAJOR!, jdtls needs Java %JDTLS_JAVA% or newer
) else (
  call :find_jdk
  if not defined JAVA if defined JAVA_HOME (
    call :version_of "%JAVA_HOME%"
    if defined MAJOR_KNOWN if !MAJOR! GEQ %JDTLS_JAVA% set "JAVA=%JAVA_HOME%"
  )
)

set "JDTLS=%IDE_JDTLS_HOME%"
if not defined JDTLS call :find_jdtls "%BASE%tools"
if not defined JDTLS if defined IDE_TOOLS call :find_jdtls "%IDE_TOOLS%"
if not defined JDTLS if defined IDE_TOOLS call :find_jdtls "%IDE_TOOLS%\tools"

set "DEBUG=%IDE_JAVA_DEBUG_PLUGIN%"
if not defined DEBUG call :find_debug "%BASE%tools"
if not defined DEBUG if defined IDE_TOOLS call :find_debug "%IDE_TOOLS%"
if not defined DEBUG if defined IDE_TOOLS call :find_debug "%IDE_TOOLS%\tools"

if defined PRINT (
  echo IDE_JAVA_HOME=%JAVA%
  echo IDE_JDTLS_HOME=%JDTLS%
  echo IDE_JAVA_DEBUG_PLUGIN=%DEBUG%
  echo JAVA_HOME=%JAVA_HOME%
)

rem jdtls cannot start without all three, so one line says what is missing.
set "REASON="
if not defined JDTLS set "REASON=jdtls not found (tools\jdtls-* next to arro.exe and in IDE_TOOLS)"
if not defined REASON if not defined JAVA set "REASON=JDK %JDTLS_JAVA% or newer not found for jdtls (IDE_JAVA_HOME, Amazon Corretto, Program Files\Java, Eclipse Adoptium, LOCALAPPDATA\Programs, JAVA_HOME)"
if not defined REASON if not defined DEBUG set "REASON=java-debug plugin not found (vscode-java-debug-*\extension\server\com.microsoft.java.debug.plugin-*.jar)"
if defined REASON echo run-arro: %REASON%; starting without jdtls

if defined PRINT (
  if defined REASON echo STATUS=no-jdtls
  if not defined REASON echo STATUS=ready
  exit /b 0
)

if not defined REASON (
  set "IDE_JAVA_HOME=%JAVA%"
  set "IDE_JDTLS_HOME=%JDTLS%"
  set "IDE_JAVA_DEBUG_PLUGIN=%DEBUG%"
)
"%BASE%arro.exe" %*
exit /b %ERRORLEVEL%

:find_jdk
set "BEST="
set "BEST_MAJOR=0"
rem jdtls needs %JDTLS_JAVA% or newer, so older JDKs are not candidates even when they are newer
rem than the one in JAVA_HOME. The newest major of all search roots wins.
set "PROGRAM_FILES=%ProgramFiles%"
if defined IDE_PROGRAM_FILES set "PROGRAM_FILES=%IDE_PROGRAM_FILES%"
if defined PROGRAM_FILES (
  call :scan_jdk "%PROGRAM_FILES%\Amazon Corretto"
  call :scan_jdk "%PROGRAM_FILES%\Java"
  call :scan_jdk "%PROGRAM_FILES%\Eclipse Adoptium"
)
if defined LOCALAPPDATA call :scan_jdk "%LOCALAPPDATA%\Programs"
if defined BEST set "JAVA=%BEST%"
exit /b

:scan_jdk
for /f "delims=" %%D in ('dir /b /ad "%~1\jdk*" 2^>nul') do (
  call :version_of "%~1\%%D"
  if defined MAJOR_KNOWN if !MAJOR! GEQ %JDTLS_JAVA% if !MAJOR! GTR !BEST_MAJOR! (
    set "BEST_MAJOR=!MAJOR!"
    set "BEST=%~1\%%D"
  )
)
exit /b

rem Reads JAVA_VERSION from the release file. 1.8.0_492 means major 8, 17.0.20.1
rem means 17 and a bare 21 or 25 stays as is. Missing or unreadable means unknown.
:version_of
set "MAJOR="
set "MAJOR_KNOWN="
set "RELEASE=%~1\release"
if not exist "%RELEASE%" exit /b
set "VALUE="
for /f "usebackq tokens=1,* delims==" %%A in ("%RELEASE%") do (
  if /i "%%A"=="JAVA_VERSION" set "VALUE=%%~B"
)
if not defined VALUE exit /b
set "TAIL=!VALUE!"
if "!TAIL:~0,2!"=="1." set "TAIL=!TAIL:~2!"
for /f "tokens=1 delims=.-_+" %%M in ("!TAIL!") do set "MAJOR=%%M"
set "DIGITS=!MAJOR!"
for /f "delims=0123456789" %%X in ("!DIGITS!") do set "DIGITS="
if not defined DIGITS exit /b
set "MAJOR_KNOWN=1"
exit /b

:find_jdtls
for /f "delims=" %%D in ('dir /b /ad /o-n "%~1\jdtls-*" 2^>nul') do (
  if not defined JDTLS set "JDTLS=%~1\%%D"
)
exit /b

:find_debug
for /f "delims=" %%D in ('dir /b /ad /o-n "%~1\vscode-java-debug-*" 2^>nul') do (
  if not defined DEBUG (
    for /f "delims=" %%J in ('dir /b /a-d /o-n "%~1\%%D\extension\server\com.microsoft.java.debug.plugin-*.jar" 2^>nul') do (
      if not defined DEBUG set "DEBUG=%~1\%%D\extension\server\%%J"
    )
  )
)
exit /b
