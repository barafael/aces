# Credits

aces uses the following third-party assets. Their licenses require this
attribution wherever the game is shared; it is also shown in game (main
menu → `K`).

## Aircraft models

All from [Sketchfab](https://sketchfab.com), in `aces-client/assets/models/`
(stored with Git LFS). The model files are used as downloaded, except the
MiG-21 (see below); the game scales, rotates and re-centres them when
loading (`flight::model_fixup`).

| Model | Author | License |
|---|---|---|
| [F-15E Strike Eagle - Fighter Jet - Free](https://sketchfab.com/3d-models/f-15e-strike-eagle-fighter-jet-free-fff7d75490474e9b964d90cc031c8d01) | [bohmerang](https://sketchfab.com/bohmerang) | [CC BY-NC-SA 4.0](https://creativecommons.org/licenses/by-nc-sa/4.0/) |
| [F / A-141F fighter](https://sketchfab.com/3d-models/f-a-141f-fighter-bb4fa6ed9fef4a52b119e80748327276) | [小微流 (jiagoushi)](https://sketchfab.com/jiagoushi) | [CC BY-NC-SA 4.0](https://creativecommons.org/licenses/by-nc-sa/4.0/) |
| [Mikoyan-gurevich mig-19](https://sketchfab.com/3d-models/mikoyan-gurevich-mig-19-056bde58c01345c59793aaac7e1764bc) | [Chenchanchong](https://sketchfab.com/Chenchanchong) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [Mig-23 MLD](https://sketchfab.com/3d-models/mig-23-mld-7a13c91f07e042a685b4d265644fdc06) | [Tim Samedov (citizensnip)](https://sketchfab.com/citizensnip) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [JAS39  Gripen](https://sketchfab.com/3d-models/jas39-gripen-a2b70c2f92af45d18d95f02b60621dbf) | [helijah](https://sketchfab.com/helijah) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [Northrop T-38 Talon](https://sketchfab.com/3d-models/northrop-t-38-talon-d5d22b4b37944f0e9bb1e77ede028f2f) | [helijah](https://sketchfab.com/helijah) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [Su-25](https://sketchfab.com/3d-models/su-25-88b71eb848cf4418a95dff497c07cefc) | [tnikita](https://sketchfab.com/tnikita) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [MIG-21 Fishbed - cold war era fighter - free](https://sketchfab.com/3d-models/mig-21-fishbed-cold-war-era-fighter-free-18927e007c3b47ed9e676f88b4adb578) | [NETRUNNER_pl](https://sketchfab.com/NETRUNNER_pl) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [F-14 TOMCAT](https://sketchfab.com/3d-models/f-14-tomcat-082e081ecea94a6aaa8c7bb72ec9136b) | [Ryan.Qin](https://sketchfab.com/Ryan.Qin) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [F16-C Falcon](https://sketchfab.com/3d-models/f16-c-falcon-4bc2ff75dc584af2afd0aa6bd8b79015) | [Carlos.Maciel](https://sketchfab.com/Carlos.Maciel) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [Eurofighter Typhoon Game Prop](https://sketchfab.com/3d-models/eurofighter-typhoon-game-prop-01d9a26a89dc4a17a9fa4c4c1f7ac39f) | [robnewman76](https://sketchfab.com/robnewman76) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [Lockheed SR-71 "Blackbird"](https://sketchfab.com/3d-models/lockheed-sr-71-blackbird-e2400e6119f5414c89e075654a82d30a) | [KOG_THORNS (ioai25312)](https://sketchfab.com/ioai25312) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [Mig-15](https://sketchfab.com/3d-models/mig-15-db02d092b7a344339a3e60d2dc0f6f39) | [Vermishel](https://sketchfab.com/Vermishel) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [Northrop F-5 Freedom Fighter](https://sketchfab.com/3d-models/northrop-f-5-freedom-fighter-8074c87c10fa47ef909bac55d21cc789) | [Pan_Ar4ik (rave.Ar4ik)](https://sketchfab.com/rave.Ar4ik) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [super-etendard](https://sketchfab.com/3d-models/super-etendard-3589004316f54dba90ed7f34455eeeb2) | [helijah](https://sketchfab.com/helijah) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| [Su-47 Berkut](https://sketchfab.com/3d-models/su-47-berkut-4a2b1cecf13c4c9db7933ffd7fd67339) | [Carlos.Maciel](https://sketchfab.com/Carlos.Maciel) | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |

The two CC BY-NC-SA 4.0 models may not be used commercially, and modified
versions of them must be shared under the same license. The licenses are as
declared by the uploaders on the model pages and in the files' metadata.

Changes: the MiG-21 file shipped three aircraft (two of them in flight
with smoke trails); aces keeps only the first, parked one (`master 1`) — its
scene root lists that node alone, everything else in the file is unchanged.

## Font

**FreeSans Bold** from [GNU FreeFont](https://www.gnu.org/software/freefont/),
Copyleft 2002, 2003, 2005 Free Software Foundation, used under the
[GNU General Public License](https://www.gnu.org/copyleft/gpl.html) (the
license the font file itself declares). In `aces-client/assets/fonts/`.
