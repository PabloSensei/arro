Arro
====

Version: {{COMMIT}}
Date: {{DATE}}

An early preview for Windows 11 x86-64. Arro is not ready for everyday work yet:
features change, and settings and data may move between versions. Most of Arro's
code is written by AI and reviewed by a human.

This folder holds arro.exe, helpers/, tools/, run-arro.cmd, this file,
THIRD-PARTY.txt and NOTICE.txt. There is no installer: unpack the folder anywhere.
Arro keeps its settings, plugins, state, caches and logs outside this folder, in
%APPDATA%\Arro and %LOCALAPPDATA%\Arro.

What is in the folder
---------------------

  arro.exe         the Arro program; it finds the JDK, jdtls and java-debug itself;
  helpers/         jar files for the project's JVM: maven-ext.jar and ide-test-listener.jar;
  tools/           jdtls 1.63.0 and java-debug 0.60.0, ready to use;
  run-arro.cmd     an optional starter that sets the same paths as variables;
  README.txt       this file;
  THIRD-PARTY.txt  the third-party crates and their licenses;
  NOTICE.txt       notices and license texts of third-party components.

When conpty.dll and OpenConsole.exe sit next to arro.exe and match the pinned hashes
(conpty.lock in the sources), the terminal uses that ConPTY; otherwise it uses the
system ConPTY. These two files are left out of the archive when they were not present
at build time.

What you need besides this folder
---------------------------------

Only a JDK for jdtls: JDK 25 or newer, which runs jdtls 1.63. jdtls and java-debug
are already inside tools/, so they do not have to be installed separately. The JDK
does not have to be the project's JDK: the project builds on its own JDK, and jdtls
runs on the JDK 25+ that is found.

Download Eclipse Temurin 25 for Windows x64 from

  https://adoptium.net/temurin/releases/?version=25&os=windows&arch=x64&package=jdk

The installer puts it in C:\Program Files\Eclipse Adoptium\jdk-25... by default.
Arro searches that folder, C:\Program Files\Amazon Corretto, C:\Program Files\Java
and %LOCALAPPDATA%\Programs, then JAVA_HOME when it is 25 or newer. A JDK named by
IDE_JAVA_HOME wins over the search.

Without a JDK 25+ Arro still starts: the editor, search, the project and the terminal
work, while the Java features that need the server are unavailable. The status bar
then says which piece was not found; a click on that line opens a balloon with a
"Download JDK 25" link and a "Check again" button. Check again searches once more,
and when the JDK is there it starts jdtls without restarting Arro.

The bundled servers live in

  tools\jdtls-1.63.0\
  tools\vscode-java-debug-0.60.0\extension\server\com.microsoft.java.debug.plugin-0.53.2.jar

arro.exe looks for them itself, in tools\ next to arro.exe first, then in the folder
named by IDE_TOOLS, then in IDE_TOOLS\tools. Exact paths can be set with
IDE_JDTLS_HOME and IDE_JAVA_DEBUG_PLUGIN, and the JDK for jdtls with IDE_JAVA_HOME; a
variable that is set wins over the search. run-arro.cmd sets the same variables after
its own search, so starting arro.exe directly, from a shortcut or from the taskbar
works the same. jdtls needs the folders bin, features, plugins and config_win of its
release on Windows; config_linux*, config_mac* and config_ss_* are not needed and are
left out of this folder.

Starting
--------

  arro.exe                    open the welcome screen; a double click works too;
  arro.exe C:\work\shop       open a project folder;
  arro.exe C:\work\shop\src\Main.java
                              open a file in its folder;
  run-arro.cmd                the same commands after a search for the tools;
  run-arro.cmd --print-paths  print the paths that were found and exit.

Variables that Arro reads, and that run-arro.cmd reads and sets:

  IDE_JAVA_HOME          the JDK for jdtls, 25 or newer;
  IDE_JDTLS_HOME         the jdtls folder;
  IDE_JAVA_DEBUG_PLUGIN  the java-debug jar;
  IDE_TOOLS              another folder with jdtls-* and vscode-java-debug-*;
  JAVA_HOME              the project's JDK, used as is.

The JDK for jdtls is chosen as follows: IDE_JAVA_HOME when it is set (taken as is,
with a warning from run-arro.cmd when it is older than 25); otherwise the newest JDK 25+
in C:\Program Files\Amazon Corretto\jdk*, C:\Program Files\Java\jdk*,
C:\Program Files\Eclipse Adoptium\jdk* and %LOCALAPPDATA%\Programs\jdk*; then
JAVA_HOME, only when it is 25+. The version is read from the release file at the root
of the JDK. When no JDK 25+ is found, one line says why, and Arro starts without
jdtls. Arro does not change JAVA_HOME: the project's Maven and Gradle see it as is.

jdtls is the newest jdtls-* folder found, and java-debug is the newest plugin jar in
the newest vscode-java-debug-* folder. Both are looked for next to arro.exe first.

Features
--------

Window and project:

  * a welcome screen with recent projects, opening a folder, creating a Maven or
    Gradle project and cloning from version control;
  * the project tree: new files, folders, Java classes, interfaces, enums, records
    and packages, rename and delete, select the current file;
  * tabs, split groups, closing and reopening tabs;
  * Project, Commit, Structure, Terminal, Run, Debug, Problems, Usages, TODO,
    Services, Tasks and AI Assistant panels, which can be moved and resized;
  * themes, settings, user plugins and the local history of files.

Search and navigation:

  * Search Everywhere over files, classes, symbols and actions;
  * Go to File, Go to Class, Go to Symbol, Find Action, Recent Files, Switcher;
  * Find in Files and Replace in Files, File Structure, Back and Forward,
    Recent Locations.

Editor:

  * tree-sitter highlighting, auto-indent, folding, soft wrap and whitespace;
  * multiple carets, select next occurrence, move and duplicate lines, comments,
    bookmarks;
  * find and replace in a file, go to line, live templates;
  * completion, parameter info, hover, go to declaration and implementation,
    diagnostics, intentions, rename, find usages, refactorings and formatting
    through jdtls;
  * delayed saving and undo/redo with grouped edits.

Build, run and debug:

  * Maven and Gradle: project sync, tasks, running the application and tests;
  * Run Test at Caret, Rerun Failed Tests, a console with ANSI colors;
  * breakpoints with conditions and hit counts, step over/into/out, resume,
    evaluate, Attach to JVM and the breakpoint list, through java-debug.

Git, terminal and services:

  * changes, commit, push, update project, revert, branches and history;
  * a built-in ConPTY terminal;
  * a Docker panel with containers and logs;
  * Problems, Usages, TODO, Structure and local history.

Keys
----

The default keymap follows IntelliJ IDEA, so moving over is easy.

Window and panels:

  Alt+1                  Project
  Alt+2                  AI Assistant
  Alt+3                  Usages
  Alt+4                  Run
  Alt+5                  Debug
  Alt+6                  Problems
  Alt+7                  Structure
  Alt+8                  Services
  Alt+0                  Commit
  Alt+F12                Terminal
  Ctrl+Alt+S             Settings
  Ctrl+Alt+T             in the editor Surround With (live templates),
                         elsewhere Toggle Theme
  Ctrl+F4                Close Tab
  Ctrl+Shift+T           Reopen Tab
  Alt+Left, Alt+Right    Previous/Next Tab
  Ctrl+Alt+Shift+Left    Focus Previous Group
  Ctrl+Alt+Shift+Right   Focus Next Group
  F2, Shift+F2           Next/Previous Problem
  Alt+F1                 Select in Project

Search and navigation:

  Double Shift           Search Everywhere
  Double Ctrl            Run Anything
  Ctrl+Shift+N           Go to File
  Ctrl+N                 Go to Class
  Ctrl+Alt+Shift+N       Go to Symbol
  Ctrl+Shift+A           Find Action
  Ctrl+E                 Recent Files
  Ctrl+Tab               Switcher
  Ctrl+Shift+E           Recent Locations
  Ctrl+Shift+F           Find in Files
  Ctrl+Shift+R           Replace in Files
  Ctrl+F12               File Structure
  Ctrl+Alt+Left          Back
  Ctrl+Alt+Right         Forward

Editor:

  Ctrl+Space             Code Completion
  Ctrl+P                 Parameter Info
  Ctrl+B                 Go to Declaration
  Ctrl+Alt+B             Go to Implementation
  Ctrl+Q                 Quick Documentation
  Alt+Enter              Show Intention Actions
  Ctrl+F                 Find
  Ctrl+R                 Replace
  F3, Shift+F3           Find Next/Previous
  Ctrl+G                 Go to Line
  Ctrl+S                 Save All
  Ctrl+Z, Ctrl+Shift+Z   Undo/Redo
  Ctrl+D                 Duplicate Line
  Ctrl+Y                 Delete Line
  Alt+Shift+Up/Down      Move Line Up/Down
  Ctrl+/                 Comment by Line Comment
  Ctrl+Shift+/           Comment by Block Comment
  Ctrl+W, Ctrl+Shift+W   Extend/Shrink Selection
  Alt+J, Alt+Shift+J     Select/Unselect Occurrence
  Ctrl+Alt+Shift+J       Select All Occurrences
  Alt+Shift+Insert       Column Mode
  Ctrl+Shift+V           Paste History
  Ctrl+Shift+Enter       Complete Statement
  Ctrl+J                 Insert Live Template
  F11                    Toggle Bookmark
  Ctrl+F11               Toggle Bookmark with Mnemonic
  Shift+F11              Show Bookmarks
  Ctrl+Alt+L             Reformat Code
  Ctrl+Alt+O             Optimize Imports

Java and refactorings:

  Alt+F7                 Find Usages
  Shift+F6               Rename
  Ctrl+Alt+M             Extract Method
  Ctrl+Alt+V             Introduce Variable
  Ctrl+Alt+F             Introduce Field
  Ctrl+Alt+C             Introduce Constant
  Ctrl+Alt+P             Introduce Parameter
  Ctrl+Alt+N             Inline
  Ctrl+Alt+Shift+T       Refactorings
  F6                     Move
  Ctrl+F6                Change Signature
  Alt+Insert             Generate
  Ctrl+Shift+P           Expression Type
  Ctrl+U                 Go to Super Method
  Ctrl+H                 Type Hierarchy

Run and debug:

  Shift+F10              Run
  Shift+F9               Debug
  Ctrl+Shift+F10         Run Test at Caret
  F8                     Step Over
  F7                     Step Into
  Shift+F8               Step Out
  F9                     Resume
  Alt+F8                 Evaluate
  Ctrl+F8                Toggle Breakpoint
  Ctrl+Shift+F8          View Breakpoints
  Ctrl+F2                Stop

Git:

  Ctrl+K                 Commit
  Ctrl+Shift+K           Push Changes
  Ctrl+T                 Update Project
  Ctrl+Alt+Z             Revert Change

Licenses and third-party components
-----------------------------------

Arro is licensed under FSL-1.1-ALv2, the Functional Source License 1.1 with the
Apache License 2.0 as the future license. THIRD-PARTY.txt lists the crates with their
versions and licenses. The bundled tools\jdtls-1.63.0 (EPL-2.0) and
tools\vscode-java-debug-0.60.0 (the plugin is EPL-1.0 and the extension is MIT, with
commons-io, RxJava and Reactive Streams inside) are listed there with links to their
sources. The full EPL-2.0 text is in tools\jdtls-1.63.0\epl-2.0.html; the EPL-1.0
and MIT texts of java-debug are in its extension\EPL-1.0.txt and extension\LICENSE.txt. NOTICE.txt holds those texts, the
Lucide license for the icons, the intellij-community notice for the default keymap,
the OFL texts of the Inter and JetBrains Mono fonts and, when conpty.dll and
OpenConsole.exe are shipped, the MIT license of Windows Terminal.

IntelliJ IDEA and JetBrains are trademarks of JetBrains s.r.o. Arro is not affiliated
with JetBrains; the build contains no JetBrains or IntelliJ IDEA logos.

Logs and crash reports
----------------------

Arro writes its logs to %LOCALAPPDATA%\Arro\logs. When the program ends unexpectedly,
the newest file there is what names the reason:

  panic-<milliseconds>.log   a panic, with the message and the stack;
  crash-<milliseconds>.log   an access violation, a stack overflow or an abort;
  crash-<milliseconds>.dmp   a minidump for the same crash.

The first lines of a panic or crash log hold the version, the build commit and the
Windows version, so the log can be matched with the build that wrote it. When Arro
crashes, send the newest panic-*.log or crash-*.log together with the matching
crash-*.dmp, when it exists, and say what you did just before the crash. Do not send
the whole folder: the logs of older crashes are not needed.

Report problems and ideas at https://github.com/PabloSensei/arro/issues.
