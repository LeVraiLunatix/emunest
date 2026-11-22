; Installeur Inno Setup d'EmuNest — compilé par la CI : ISCC /DAppVersion=0.1.0 installer\emunest.iss
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6E1B7F52-7C0A-4D6B-9B63-EMUNEST00001}
AppName=EmuNest
AppVersion={#AppVersion}
AppPublisher=LeVraiLunatix
DefaultDirName={autopf}\EmuNest
DefaultGroupName=EmuNest
DisableProgramGroupPage=yes
; installation pour l'utilisateur seul : pas de droits administrateur, mises à jour automatiques possibles
PrivilegesRequired=lowest
OutputDir=Output
OutputBaseFilename=EmuNest-Setup-{#AppVersion}
SetupIconFile=..\app\icon.ico
UninstallDisplayIcon={app}\emunest.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Languages]
Name: "french"; MessagesFile: "compiler:Languages\French.isl"

[Tasks]
Name: "desktopicon"; Description: "Créer un raccourci sur le Bureau"; Flags: unchecked

[Files]
Source: "..\target\release\emunest.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\EmuNest"; Filename: "{app}\emunest.exe"
Name: "{autodesktop}\EmuNest"; Filename: "{app}\emunest.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\emunest.exe"; Description: "Lancer EmuNest"; Flags: nowait postinstall skipifsilent
; les données (bibliothèque, jaquettes, émulateurs) restent dans ~\.emunest : la désinstallation ne les supprime pas
