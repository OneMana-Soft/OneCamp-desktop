# OneCamp for the desktop

OneCamp in its own window, for Windows, macOS and Linux. Chat, docs, tasks,
calls and AI teammates from your workspace, with native notifications and a
tray icon that keeps it running so messages still reach you.

**Download:** [the latest release](https://github.com/OneMana-Soft/OneCamp-desktop/releases/latest).

| Your computer | File |
|---|---|
| Windows | `OneCamp_x.y.z_x64-setup.exe` |
| Mac with Apple silicon (M1 and later) | `OneCamp_x.y.z_aarch64.dmg` |
| Mac with Intel | `OneCamp_x.y.z_x64.dmg` |
| Linux | `.AppImage`, `.deb` or `.rpm` |

## Using it

1. Open OneCamp and enter your workspace's address, the one you open in your
   browser, for example `onecamp.yourcompany.com`. No workspace yet? Choose
   **Try the live demo**.
2. Sign in as usual. Google and single sign-on work inside the app.
3. Closing the window keeps OneCamp in the tray. To quit, use the tray icon's
   menu. **Change workspace…** and **Check for updates** are there too.

The app holds no copy of OneCamp: it opens your workspace, so it always matches
the version your server runs. Links to other sites open in your browser.

### Dictation that stays on your computer

In the app, **Dictate** (the microphone in the AI panel, or in the composer's
AI menu) transcribes on your own computer: the audio never leaves it, and it
works even if your workspace has no speech engine set up. The first time, choose
**Set up dictation**: the app downloads a speech model of about 690 MB
([Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3),
25 European languages) and ONNX Runtime, each checked against a pinned
checksum. While it listens, the tray icon says **OneCamp is listening…**.

It needs a Mac with Apple silicon, or Windows or Linux on x64 or ARM. On an
Intel Mac, dictation uses your workspace's speech engine when it has one.

The installers are not yet code-signed. Windows may show "Windows protected
your PC": choose **More info**, then **Run anyway**. On a Mac, if it says the
app cannot be opened, right-click it and choose **Open**.

## Building

Requirements: Rust (stable), Node 20 and pnpm, plus on Linux
`libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libasound2-dev`.

```
pnpm install
pnpm dev      # run it
pnpm build    # installers in src-tauri/target/release/bundle
cd src-tauri && cargo test
```

Releases are built by `.github/workflows/release.yml` when a `v*` tag is pushed.

## Security

- Only the bundled setup page may change which workspace the app opens.
- Your workspace's pages may show notifications and use on-device dictation,
  and can do nothing else through the app. No other site gets either.
- Any new window a page asks for opens in your browser instead.
- Updates are signed, and the app refuses one whose signature does not match.

Report security problems to support@onemana.dev.

## Licence

MIT. The OneCamp server is at https://github.com/OneMana-Soft/OneCamp and the
web app at https://github.com/OneMana-Soft/OneCamp-fe.

Dictation follows the approach of [Handy](https://github.com/cjpais/Handy)
(MIT) and runs NVIDIA's Parakeet TDT 0.6B v3, used under
[CC-BY-4.0](https://creativecommons.org/licenses/by/4.0/), through
[transcribe-rs](https://crates.io/crates/transcribe-rs) and the ONNX export by
[istupakov](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx).
