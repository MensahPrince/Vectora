; Inno Setup script for the Cutlass Windows installer.
;
; This compiles the staged payload produced by scripts/package-windows.ps1
; (cutlass-desktop.exe + licenses + README) into a single Setup.exe with
; Start-menu shortcut, optional desktop icon, and uninstaller.
;
; Do not run this file directly; use scripts/package-windows-installer.ps1,
; which builds, stages, and invokes ISCC with the right defines. To compile
; by hand:
;
;   iscc /DMyAppVersion=0.5.3-alpha.0 ^
;        "/DMySourceDir=C:\path\to\dist\staging-windows-x86_64\cutlass-0.5.3-alpha.0-windows-x86_64" ^
;        "/DMyOutputDir=C:\path\to\dist" ^
;        packaging\windows\cutlass.iss

#define MyAppName "Cutlass"
#define MyAppExeName "cutlass-desktop.exe"
#define MyAppPublisher "Cutlass"
#define MyAppURL "https://github.com/1Mr-Newton/cutlass"

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif

#ifndef MySourceDir
  #define MySourceDir "..\..\dist\staging-windows-x86_64\cutlass-" + MyAppVersion + "-windows-x86_64"
#endif

#ifndef MyOutputDir
  #define MyOutputDir "..\..\dist"
#endif

#ifndef MyOutputBaseFilename
  #define MyOutputBaseFilename "Cutlass-" + MyAppVersion + "-windows-x86_64-Setup"
#endif

; Inno Setup architecture identifier: "x64compatible" or "arm64".
#ifndef MyArchAllowed
  #define MyArchAllowed "x64compatible"
#endif

[Setup]
; AppId uniquely identifies this product for upgrades/uninstall. Keep it stable.
AppId={{B6E6F3C2-7F4D-4E2A-9C3E-9A0F2C1D4E7B}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
LicenseFile={#MySourceDir}\LICENSE-MIT
OutputDir={#MyOutputDir}
OutputBaseFilename={#MyOutputBaseFilename}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Cutlass ships only as 64-bit; install into the native Program Files.
ArchitecturesAllowed={#MyArchAllowed}
ArchitecturesInstallIn64BitMode={#MyArchAllowed}
UninstallDisplayIcon={app}\{#MyAppExeName}
#ifdef MyAppIcon
SetupIconFile={#MyAppIcon}
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#MySourceDir}\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent
