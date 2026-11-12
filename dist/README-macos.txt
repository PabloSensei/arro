Arro for macOS
==============

Version: {{COMMIT}}
Date: {{DATE}}

An early preview for macOS. Arro is not ready for everyday work yet: features change,
and settings and data may move between versions. Most of Arro's code is written by AI
and reviewed by a human.

This folder holds Arro.app, this file, THIRD-PARTY.txt and NOTICE.txt. Inside the
bundle, Contents/MacOS/arro is the program and Contents/Resources holds the language
tools, the notices, this file and the icon. There is no installer: drag Arro.app into
the Applications folder, or run it from where it is unpacked.

The first start of a build without a signature of an Apple developer
--------------------------------------------------------------------

The builds here are signed ad hoc: macOS accepts the program, but it cannot prove who
built it, and Gatekeeper stops the first start with a message that Arro cannot be
opened because the developer cannot be verified. That is expected for a preview.

To open it once:

  1. Open the folder with Arro.app in the Finder.
  2. Control-click Arro.app (or right-click it) and choose Open.
  3. In the dialog choose Open again.

macOS remembers the choice for this copy of the bundle. The same from a terminal:

  xattr -dr com.apple.quarantine /Applications/Arro.app

Do this only for a build you downloaded from a place you trust. Nothing has to be
turned off in System Settings for the whole system.

What you need besides this bundle
---------------------------------

Only a JDK for jdtls: JDK 25 or newer, which runs jdtls 1.63. jdtls and java-debug are
already inside the bundle, so they do not have to be installed separately. The JDK does
not have to be the project's JDK: the project builds on its own JDK, and jdtls runs on
the JDK 25+ that is found.

Download Eclipse Temurin 25 for macOS from

  https://adoptium.net/temurin/releases/?version=25&os=mac&arch=aarch64&package=jdk

Arro searches /Library/Java/JavaVirtualMachines, the folders of Temurin and Corretto
there, and then JAVA_HOME when it is 25 or newer. A JDK named by IDE_JAVA_HOME wins over
the search.

Without a JDK 25+ Arro still starts: the editor, search, the project and the terminal
work, while the Java features that need the server are unavailable. The status bar then
says which piece was not found; a click on that line opens a balloon with a "Download
JDK 25" link and a "Check again" button.

The bundled servers live in

  Arro.app/Contents/Resources/tools/jdtls-1.63.0/
  Arro.app/Contents/Resources/tools/vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar

Starting
--------

  open Arro.app                      the welcome screen;
  open Arro.app --args /work/shop    a project folder;
  open Arro.app --args /work/shop/src/Main.java
                                     a file in its folder.

Variables that Arro reads:

  IDE_JAVA_HOME          the JDK for jdtls, 25 or newer;
  IDE_JDTLS_HOME         the jdtls folder;
  IDE_JAVA_DEBUG_PLUGIN  the java-debug jar;
  IDE_TOOLS              another folder with jdtls-* and vscode-java-debug-*;
  JAVA_HOME              the project's JDK, used as is.

Keys
----

The keymap follows the IntelliJ IDEA keymap of macOS: Command where IDEA uses Command,
Control where IDEA keeps it.

  Cmd+C, Cmd+V, Cmd+X   Copy, Paste, Cut
  Cmd+Z, Shift+Cmd+Z    Undo, Redo
  Cmd+S                 Save All
  Cmd+O                 Go to Class
  Shift+Cmd+O           Go to File
  Cmd+E                 Recent Files
  Cmd+B                 Go to Declaration
  Cmd+N                 Generate
  Cmd+F, Cmd+R          Find, Replace
  Shift+Cmd+F           Find in Files
  Ctrl+Space            Code Completion
  Ctrl+Alt+O            Optimize Imports
  Cmd+Alt+L             Reformat Code
  Option+Return         Show Intention Actions
  Cmd+/                 Comment by Line Comment
  Cmd+Q                 Quit Arro

Where the files of Arro live
----------------------------

Settings and state are in ~/Library/Application Support/Arro, and the logs and crash
reports are in its logs folder:

  panic-<milliseconds>.log   a panic, with the message and the stack;
  crash-signals.log          a fatal signal, such as a segmentation fault.

When Arro crashes, send the newest log and say what you did just before the crash.
Report problems and ideas at https://github.com/PabloSensei/arro/issues.

Licenses and third-party components
-----------------------------------

Arro is licensed under FSL-1.1-ALv2, the Functional Source License 1.1 with the
Apache License 2.0 as the future license. THIRD-PARTY.txt lists the crates with their
versions and licenses. The bundled jdtls (EPL-2.0) and java-debug (EPL-1.0 for the
plugin, MIT for the extension) are listed there with links to their sources; the full
EPL-2.0 text is in tools/jdtls-1.63.0/epl-2.0.html and the EPL-1.0 text of java-debug
is in tools/vscode-java-debug-0.60.0/extension/EPL-1.0.txt. NOTICE.txt holds those
texts, the Lucide license for the icons, the intellij-community notice for the default
keymap and the OFL texts of the Inter and JetBrains Mono fonts.

IntelliJ IDEA and JetBrains are trademarks of JetBrains s.r.o. Arro is not affiliated
with JetBrains; the bundle contains no JetBrains or IntelliJ IDEA logos.
