<h1 align="center">
  <img src="assets/interceptor-banner.png" alt="Interceptor - packet queueing for Windows">
</h1>

<p align="center">
  <a href="https://github.com/wedontselldrugs/interceptor/releases/latest"><strong>Download</strong></a> &bull;
  <a href="https://github.com/wedontselldrugs/interceptor/issues"><strong>Report an issue</strong></a> &bull;
  <a href="https://github.com/wedontselldrugs/interceptor/pulls"><strong>Contribute</strong></a> &bull;
  <a href="LICENSE"><strong>License</strong></a>
</p>

<p align="center">
  <a href="https://github.com/wedontselldrugs/interceptor/releases/latest"><img src="https://img.shields.io/github/v/release/wedontselldrugs/interceptor?style=flat-square" alt="Latest release"></a>
  <a href="https://github.com/wedontselldrugs/interceptor/releases"><img src="https://img.shields.io/github/downloads/wedontselldrugs/interceptor/total?style=flat-square" alt="Total downloads"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/wedontselldrugs/interceptor?style=flat-square" alt="GPL-3.0-or-later license"></a>
  <a href="https://github.com/wedontselldrugs/interceptor/stargazers"><img src="https://img.shields.io/github/stars/wedontselldrugs/interceptor?style=flat-square" alt="GitHub stars"></a>
</p>

<p align="center">
  If Interceptor is useful to you, please consider starring the repo. ❤️
</p>

## About

> Testing a game server and want to see how gameplay reacts to a short burst of delayed traffic?
> **Interceptor lets you trigger that delay when it matters.**

**Interceptor** is a Windows terminal tool made for game-network testing. Set the port used by your game server, choose TCP, UDP, or both, and press your trigger key during gameplay to briefly queue matching packets before releasing them again.

It is built in Rust on top of [WinDivert](https://reqrypt.org/windivert.html). During a hold window, WinDivert routes only matching game traffic through Interceptor; those packets are held for the selected duration and then sent back out in a release burst.

## Features

- Filter by remote port and pick `TCP`, `UDP`, or both
- Trigger a hold window with any key you choose
- Use the built-in random timing (`1.8s–2.5s`) or set your own min/max
- Watch queued, released, and failed-release counts live in the terminal
- Traffic outside an active hold window is never touched
- Held traffic is capped at `4096` packets or `16 MiB` - anything over that passes through normally

## Installation

Interceptor supports **Windows x64** and requires administrator access so WinDivert can load its signed network driver.

1. Download the latest release from
   [GitHub Releases](https://github.com/wedontselldrugs/interceptor/releases/latest).
2. Extract the into a folder.
3. Run `interceptor.exe` and approve the administrator prompt.

Your extracted folder should look like this:

```text
interceptor.exe
WinDivert.dll
WinDivert64.sys
LICENSE
README.md
WinDivert-LICENSE.txt
```

For the best experience, use Windows Terminal or a recent PowerShell window.

## Usage

1. Open **Settings** and enter the remote port your server or game uses
2. Select whether that traffic uses `TCP`, `UDP`, or both.
3. Choose a trigger key and either the default or a custom hold window.
4. Start Interceptor.
5. Press the trigger while the traffic you intend to test is active.

Press `q` or `Esc` to exit cleanly - any held packets get released first. Use `Ctrl+C` if you need to force-quit.
 
When idle, Interceptor sits quietly and doesn't touch your traffic. When triggered, it opens a short capture session, holds the packets, releases them, and closes again.

### Hold Windows

The **default** hold window picks a random duration between `1.8s` and `2.5s`. With a **custom** window you can set your own min and max.

## How It Works

```text
                              +-----------------------+
                              |                       |
                     +------->|    interceptor.exe    |--------+
                     |        |   hold + release UI   |        |
                     |        +-----------------------+        |
                     |                                         |
                     | matching selected port/protocol         | queued packet
                     | during an active hold window            | re-injected
 [user mode]         |                                         |
 ....................|.........................................|...................
 [kernel mode]       |                                         |
                     |                                         |
              +---------------+                                +----------------->
  packet      |               | non-matching or idle traffic
 ------------>| WinDivert.sys |-------------------------------------------------->
              |               |
              +---------------+
```
 
The configured port is the remote endpoint. Outbound packets are matched by destination port; inbound responses are matched by source port.

## Troubleshooting

| Problem | Likely cause | Fix |
|---|---|---|
| `WinDivertOpen` fails with error `5` | App wasn't run as admin | Restart and accept the UAC prompt |
| `WinDivert.dll` not found | The DLL was moved away from the exe | Keep all files in the same folder |
| No packets are queued | Wrong port or protocol selected | Double-check the remote port and TCP/UDP setting |
| Packets fail to release | WinDivert or the network stack rejected reinjection | Check the dashboard for errors and restart the session |
| Queue limit hit | Too much traffic for one hold window | Shorten your hold window or narrow the traffic |
| Antivirus warning | Packet drivers look suspicious to security tools | WinDivert warning, based on reputation. You can add it to AV exclusions |

## Safety and Privacy

Interceptor is meant for **local testing and research** on systems you own or have permission to test. Don't use it to disrupt services, interfere with other people, or get around anti-cheat systems.
 
Because it uses WinDivert and reinjects traffic, antivirus or anti-cheat software may flag it. **Interceptor has no stealth or bypass features, don't run it alongside anti-cheat-protected games.**
 
Interceptor does not collect any data, send analytics, or upload packet contents.

## Building from source
 
You'll need Rust for Windows and the MSVC build tools. The repo includes the WinDivert import library; the runtime files still need to sit next to the final executable.

```powershell
cargo build --release
```

The executable is written to:

```text
target\release\interceptor.exe
```

To package a release zip, run:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\package-release.ps1 `
  -WinDivertPath "C:\path\to\WinDivert\x64" `
  -WinDivertLicensePath "C:\path\to\WinDivert\LICENSE"
```

## Getting Involved

Contributions are welcome. Useful improvements include safer failure handling, better terminal layouts, documentation fixes, packaging improvements, and additional testing around the packet hold/release lifecycle.

- Open an [issue](https://github.com/wedontselldrugs/interceptor/issues) for a bug report or feature discussion.
- Submit a [pull request](https://github.com/wedontselldrugs/interceptor/pulls) with code or documentation changes.
- Star the project if it has been useful in your testing workflow.

## License
 
Interceptor is licensed under the [GNU General Public License v3.0 or later](LICENSE). You're free to use, study, modify, and share it. If you distribute a modified version, you must release the source under the same license.
 
WinDivert is redistributed under its own open-source license and remains the work of its authors.
