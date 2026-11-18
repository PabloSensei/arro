Arro for Linux
==============

Version: {{COMMIT}}
Date: {{DATE}}

An early preview for Linux x86-64. Arro is not ready for everyday work yet: features
change, and settings and data may move between versions. Most of Arro's code is written
by AI and reviewed by a human.

This folder holds arro, arro-mcp, helpers/, tools/, arro.desktop, arro-256.png,
install.sh, this file, THIRD-PARTY.txt and NOTICE.txt. Unpack the archive anywhere and
either start ./arro in place or run ./install.sh, which copies the folder into the home
directory, links arro in ~/.local/bin and puts the entry in the application menu. Arro
keeps its settings, state, caches and logs outside the program folder, in ~/.config/Arro
and ~/.local/share/Arro.

Using Arro from another program
-------------------------------

While a project is open, Arro shares the tools of its window with MCP clients. Settings,
Tools, AI, MCP Server shows the strings below with a copy button; Arro copies them and
runs none of them. Claude Code, in a terminal outside Arro:

  claude mcp add arro -- "<this folder>/arro-mcp"

Codex, in ~/.codex/config.toml:

  [mcp_servers.arro]
  command = "<this folder>/arro-mcp"

Any other MCP client, as its JSON configuration:

  {"mcpServers": {"arro": {"command": "<this folder>/arro-mcp"}}}

The client starts arro-mcp in the folder of the project, and the shim finds the window
whose project root holds that folder. Arro leaves the entry point in
~/.local/share/Arro/mcp and removes it when the window closes; only the user may read
it. An entry of a window that is no longer running is ignored.

The application menu
--------------------

install.sh installs Arro for the current user, without root:

  ./install.sh               into ~/.local/share/arro;
  ./install.sh /some/folder  into another folder;
  ./install.sh --uninstall   remove the folder, the link, the menu entry and the icons.

It copies the program and everything next to it, makes the link ~/.local/bin/arro,
writes arro.desktop into $XDG_DATA_HOME/applications (~/.local/share/applications by
default) and the icons into the hicolor theme of the same folder. The menu entry opens
the files and folders of a project with Arro, like the packages do. The databases of
the menu and of the icon theme are refreshed when update-desktop-database and
gtk-update-icon-cache are installed. No file outside the home folder is changed.

Arro updates itself in the folder install.sh put it in. Moving that folder breaks the
link and the menu entry: install again from the new place.

Packages
--------

Debian and Ubuntu have a .deb, Fedora and openSUSE an .rpm; both install into
/opt/arro and link arro from /usr/bin. A package installation belongs to the package
manager: Arro does not replace those files itself and its update window opens the
release page instead.

The AppImage is the alternative on the other distributions: chmod +x
arro-<version>-x86_64.AppImage and run it. It is one file, and it updates itself by
replacing that file.

What you need besides this folder
---------------------------------

Only a JDK for jdtls: JDK 25 or newer, which runs jdtls 1.63. jdtls and java-debug are
already inside tools/, so they do not have to be installed separately. The JDK does not
have to be the project's JDK: the project builds on its own JDK, and jdtls runs on the
JDK 25+ that is found.

Download Eclipse Temurin 25 for Linux x64 from

  https://adoptium.net/temurin/releases/?version=25&os=linux&arch=x64&package=jdk

Arro searches /usr/lib/jvm, ~/.sdkman/candidates/java, ~/.jdks and the folders under
/opt, then JAVA_HOME when it is 25 or newer. A JDK named by IDE_JAVA_HOME wins over the
search. The terminal and the language server get the same JAVA_HOME as the build.

Without a JDK 25+ Arro still starts: the editor, search, the project and the terminal
work, while the Java features that need the server are unavailable. The status bar then
says which piece was not found; a click on that line opens a balloon with a "Download
JDK 25" link and a "Check again" button.

The Database window keeps the passwords of its sources in the Secret Service of the
desktop and needs the `secret-tool` program of the `libsecret-tools` package for it, plus
a D-Bus session. Without them the window names the package and keeps the Save password
flag off.

The bundled servers live in

  tools/jdtls-1.63.0/
  tools/vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar

arro looks for them next to itself, then in the folder named by IDE_TOOLS, then in
IDE_TOOLS/tools. Exact paths can be set with IDE_JDTLS_HOME, IDE_JAVA_DEBUG_PLUGIN and
IDE_JAVA_HOME. jdtls needs bin, features, plugins and config_linux; the configuration
folders of other systems are not part of this archive.

Starting
--------

  ./arro                       open the welcome screen;
  ./arro /work/shop            open a project folder;
  ./arro /work/shop/src/Main.java
                               open a file in its folder.

Variables that Arro reads:

  IDE_JAVA_HOME          the JDK for jdtls, 25 or newer;
  IDE_JDTLS_HOME         the jdtls folder;
  IDE_JAVA_DEBUG_PLUGIN  the java-debug jar;
  IDE_TOOLS              another folder with jdtls-* and vscode-java-debug-*;
  JAVA_HOME              the project's JDK, used as is.

Keys
----

The keymap follows IntelliJ IDEA, as on Windows:

  Ctrl+Shift+N           Go to File
  Ctrl+N                 Go to Class
  Ctrl+Shift+A           Find Action
  Ctrl+Shift+F           Find in Files
  Ctrl+Space             Code Completion
  Ctrl+B                 Go to Declaration
  Ctrl+Q                 Quick Documentation
  Alt+Enter              Show Intention Actions
  Ctrl+Alt+L             Reformat Code
  Alt+F7                 Find Usages
  Shift+F6               Rename
  Shift+F10              Run
  Shift+F9               Debug
  Alt+F12                Terminal

Limitations of this preview
---------------------------

- The package is not signed and is not part of a distribution repository: the .deb and
  the .rpm come from the release page only. There is no Flatpak yet.
- The window is drawn through Wayland or X11 and needs libfontconfig, libwayland,
  libx11, libxcb and libxkbcommon at run time. On a headless machine Arro does not start.
- The integrated terminal uses the system pseudo terminal of Linux; ConPTY of Windows is
  not part of this archive.
- Arro updates itself from the Linux tar.gz of the release. Keep the folder writable, or
  the update stops with a message instead of replacing files.
- Only x86-64 is built. ARM builds are not part of this release.

Logs
----

The logs and crash reports are in ~/.local/share/Arro/logs. When Arro ends unexpectedly,
the newest panic-*.log or crash-*.log names the reason; send it and say what you did just
before the crash.

Report problems and ideas at https://github.com/PabloSensei/arro/issues.

Licenses and third-party components
-----------------------------------

Arro is free software under the GNU General Public License, version 3 or later;
LICENSE.txt holds its text, and the source is at https://github.com/PabloSensei/arro.
THIRD-PARTY.txt lists the crates with their
versions and licenses. The bundled tools/jdtls-1.63.0 (EPL-2.0) and
tools/vscode-java-debug-0.60.0 (the plugin is EPL-1.0 and the extension is MIT, with
commons-io, RxJava and Reactive Streams inside) are listed there with links to their
sources. The full EPL-2.0 text is in tools/jdtls-1.63.0/epl-2.0.html; the EPL-1.0 text
of java-debug is in its extension/EPL-1.0.txt. NOTICE.txt holds those texts, the Lucide
license for the icons, the intellij-community notice for the default keymap and the OFL
texts of the Inter and JetBrains Mono fonts.

IntelliJ IDEA and JetBrains are trademarks of JetBrains s.r.o. Arro is not affiliated
with JetBrains; the build contains no JetBrains or IntelliJ IDEA logos.
