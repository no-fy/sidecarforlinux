# xtool + GitHub Actions (unsigned IPA) + free Apple ID sideloading (xtool on Arch / SideStore)

Research date: 2026-10-30. Everything below was checked against live sources: GitHub API and raw source for xtool,
runner-images, SideStore, iloader and LiveContainer; Apple developer pages; real workflow runs. Each claim has a URL.
Anything I couldn't verify is labelled **UNVERIFIED** or given a confidence level.

---

## 0. TL;DR

* **xtool 1.24.0** (released 2026-10-26) is current. Linux ships as an AppImage (x86_64/aarch64). macOS ships as a notarized `xtool.app`, either from the release zip or through the Homebrew cask `xtool-org/tap/xtool`. Windows works through WSL.
* **Unsigned builds need no Apple ID.** `xtool dev build --ipa` without `--sign` never touches auth. On macOS it doesn't need `xtool setup` or an SDK either. The maintainer confirms it is the supported way to build without signing ([issue #194](https://github.com/xtool-org/xtool/issues/194)). xtool's own integration test runs `xtool new --skip-setup MyApp && xtool dev build` with no login.
* **Runner:** use the GitHub-hosted **`xcode-27`** label. It is a public preview: macOS 27.0.1 with **Xcode 27.1** as default (27.2 RC and 27.0 also installed), which ships **Swift 6.4** and the **iOS 27.1 SDK**. The GA alternative is `macos-26` / `macos-latest` with Xcode 26.6, Swift 6.3 and the iOS 26.5 SDK.
* **Real projects already do this**, and their builds are green in October 2026. One is `xtool dev build` on `xcode-27` in about 50 s ([tabula-app-ios](https://github.com/simon01920/tabula-app-ios/blob/main/.github/workflows/ios.yml)). Others are [MicYou-iOS](https://github.com/MicYou-Dev/MicYou-iOS/blob/swiftui/.github/workflows/build.yml), [Tanukis-Stash](https://github.com/CaramelKat/Tanukis-Stash/blob/main/.github/workflows/release.yml) and [teleprompter-studio](https://github.com/AhmedHeshamEG/teleprompter-studio/blob/master/.github/workflows/build-ipa.yml).
* **Installing on Arch with xtool:** you need `usbmuxd` plus `xtool auth login` (Apple ID password + 2FA). Then `xtool install App.ipa` re-provisions, re-signs and installs. The Darwin SDK / Xcode.xip is **not** needed just to sign and install a prebuilt IPA.
* **SideStore:** accepts the unsigned IPA and re-signs it on the device. It needs a one-time computer setup (iloader on Linux) and the **LocalDevVPN** app to be connected whenever it installs or refreshes. SideStore itself uses one of the 3 free app slots.
* **UIKit app-delegate apps work.** The `@main` class in the app library target can be a `UIApplicationDelegate`, as in [happy-ios](https://github.com/slopus/happy/blob/main/packages/happy-ios/Sources/HappyApp/AppDelegate.swift). Because the iOS 27 SDK enforces the UIScene life cycle, you must include `UIApplicationSceneManifest` in your own Info.plist. xtool doesn't generate it.
* **No restricted entitlements needed.** Bonjour advertising via `NWListener`, keychain, background modes, multiple scenes, GameController, haptics and Pencil APIs all work on a free personal team. Bonjour only needs `NSBonjourServices` and `NSLocalNetworkUsageDescription`.

---

## 1. xtool: current state

### 1.1 Releases, install methods, hosts

| Item | Value | Source |
|---|---|---|
| Latest release | **1.24.0**, 2026-10-26. Earlier: 1.23.0 (10-22), 1.22.2 (10-14), 1.21.0 (10-05) | `gh api repos/xtool-org/xtool/releases` |
| Release assets | `xtool-x86_64.AppImage`, `xtool-aarch64.AppImage` (+ `.zsync`), `xtool.app.zip`, `xtool.app.dSYM.zip` | same |
| `xtool.app.zip` 1.24.0 SHA-256 | `a56e98d69dea02a97a17e609f0ae8771c57ffaab23f1d62513fd9ada222026b3` (matches GitHub's asset digest; I verified it myself) | release API |
| Linux install | `curl -fL https://github.com/xtool-org/xtool/releases/latest/download/xtool-$(uname -m).AppImage -o xtool && chmod +x xtool && sudo mv xtool /usr/local/bin/` | [Installation-Linux](https://xtool.sh/documentation/xtooldocs/installation-linux) |
| Linux prerequisites | Swift **6.4** toolchain (only for *building*), `usbmuxd`, and Xcode **27.1** `.xip` (only for *building*; the SDK is generated from it) | same |
| macOS install | `brew install xtool-org/tap/xtool` (a cask that installs `xtool.app` and links `Contents/Resources/bin/xtool`), or the `xtool.app.zip` release | [Installation-macOS](https://xtool.sh/documentation/xtooldocs/installation-macos), [tap cask](https://github.com/xtool-org/homebrew-tap/blob/main/Casks/xtool.rb) |
| Hosts | Linux (x86_64/aarch64), Windows via WSL (with USBIPD), macOS | README |
| Arch / AUR | `xtool` 1.23.0-1 (one release behind) and `xtool-appimage` 1.19.2 (stale). Use the upstream AppImage. | AUR RPC |

On macOS, `Contents/MacOS/xtool` starts the **GUI** unless `XTL_CLI=1` is set. On a headless CI runner that hangs forever. Always call the wrapper `xtool.app/Contents/Resources/bin/xtool`, which sets `XTL_CLI=1`; the Homebrew cask links exactly this. Sources: [XToolMac.swift](https://github.com/xtool-org/xtool/blob/main/macOS/XToolMac/XToolMac.swift), [macOS/Resources/xtool](https://github.com/xtool-org/xtool/blob/main/macOS/Resources/xtool), and tabula's CI comment describing a 40-minute hang.

### 1.2 CLI commands and flags (from source, `Sources/XToolSupport/*.swift`, main @ 1.24.0)

```
xtool setup                         # = auth login (if needed) + SDK setup. On macOS the SDK step is a no-op.
xtool auth [login]  -u/--username <appleID> -p/--password <pw> -m/--mode key|password
xtool auth logout   [-r/--reset-2fa]     # --reset-2fa forgets the pseudo-device, so 2FA is asked again
xtool auth status                   # prints "Logged in." + Apple ID, Team ID, token expiry, or "Logged out"
xtool sdk [install] <Xcode.xip|Xcode.app|darwin.xtoolsdk> [--slim]   # Linux only; prints "Skipping" on macOS
xtool sdk update | remove | status
xtool sdk build <Xcode.xip|.app> <outputDir> [--arch auto|x86_64|arm64]
xtool new [<name>] [--skip-setup]   # without --skip-setup it runs setup first (login + SDK)
xtool dev [run]   -c/--configuration debug|release  [-s/--simulator (macOS only)]  [-u/--udid <udid>] [--usb|--network|--all]
xtool dev build   -c/--configuration debug|release  [-s/--sign]  [-i/--ipa]  [--triple <triple>]  (default triple arm64-apple-ios)
xtool dev generate-xcode-project    # writes xtool/<App>.xcworkspace (target/scheme "<Product>-App")
xtool dev build-server              # BSP server for SourceKit-LSP
xtool ds teams|certificates|identifiers|profiles|devices list    # Apple Developer Services queries
xtool ds anisette
xtool devices [--usb|--network|--all] [--wait|--no-wait]
xtool install  [-u/--udid <udid>] [--usb|--network|--all] <path.ipa|path.app>
xtool uninstall [-u/--udid] <bundleID>
xtool launch    [-u/--udid] <bundleID> [args...]
```

Sources: [DevCommand.swift](https://github.com/xtool-org/xtool/blob/main/Sources/XToolSupport/DevCommand.swift), [AuthCommand.swift](https://github.com/xtool-org/xtool/blob/main/Sources/XToolSupport/AuthCommand.swift), [SDKCommand.swift](https://github.com/xtool-org/xtool/blob/main/Sources/XToolSupport/SDKCommand.swift), [SetupCommand.swift](https://github.com/xtool-org/xtool/blob/main/Sources/XToolSupport/SetupCommand.swift), [NewCommand.swift](https://github.com/xtool-org/xtool/blob/main/Sources/XToolSupport/NewCommand.swift), [InstallCommand.swift](https://github.com/xtool-org/xtool/blob/main/Sources/XToolSupport/InstallCommand.swift), [ConnectionOptions.swift](https://github.com/xtool-org/xtool/blob/main/Sources/XToolSupport/ConnectionOptions.swift), [XTool.swift](https://github.com/xtool-org/xtool/blob/main/Sources/XToolSupport/XTool.swift).

Build behaviour (from [BuildSettings.swift](https://github.com/xtool-org/xtool/blob/main/Sources/PackLib/BuildSettings.swift) and [SwiftVersion.swift](https://github.com/xtool-org/xtool/blob/main/Sources/PackLib/SwiftVersion.swift)):

* **Swift ≥ 6.4** (Xcode 27.x, or the Linux 6.4 toolchain): uses `--build-system swiftbuild --triple arm64-apple-ios`.
* **Swift < 6.4**: uses SwiftPM's native build system with `--swift-sdk arm64-apple-ios`, relying on the implicit Darwin SDKs from Swift 6.1+. Swift 6.0 / Xcode 16.2 fails with this ([#386](https://github.com/xtool-org/xtool/issues/386)).
* **On macOS**, xtool invokes the real `swift` found via `xcrun -f swift` with `SDKROOT` unset, so the build isn't silently retargeted to macOS.

Outputs go to `./xtool/<Product>.app`, or `./xtool/<Product>.ipa` with `--ipa`; the `.app` is moved into the IPA's `Payload/`.

### 1.3 Exact layout produced by `xtool new <Name>` (NewCommand.swift)

```
<Name>/
├── Package.swift
├── xtool.yml
├── .gitignore            # .DS_Store /.build /Packages xcuserdata/ DerivedData/ .swiftpm/... .netrc /xtool
├── Sources/<Module>/<Module>App.swift     # SwiftUI @main App with WindowGroup { ContentView() }
├── Sources/<Module>/ContentView.swift
└── .sourcekit-lsp/config.json or .bsp/xtool.json   # LSP helper, skip with `skipLSP: true`
```

`<Module>` is `<Name>` with `-` replaced by `_`. The name must match `[A-Za-z_][A-Za-z0-9_-]*`.

`Package.swift`, verbatim:
```swift
// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "<Name>",
    platforms: [
        .iOS(.v17),
        .macOS(.v14),
    ],
    products: [
        // An xtool project should contain exactly one library product,
        // representing the main app.
        .library(
            name: "<Module>",
            targets: ["<Module>"]
        ),
    ],
    targets: [
        .target(
            name: "<Module>"
        ),
    ]
)
```

`xtool.yml`, verbatim:
```yaml
version: 1
bundleID: com.example.<Name>
```

How the app is built: the app is a **library product**. xtool generates `xtool/.xtool-tmp/Package.swift` containing one `.executableTarget("<Product>-App")`. That target holds an empty `stub.c`, depends on your library product, and links with `-rpath @executable_path/Frameworks`. The `main` symbol therefore comes from the `@main` type in your library ([Packer.swift](https://github.com/xtool-org/xtool/blob/main/Sources/PackLib/Packer.swift)). Extensions link with `-e _NSExtensionMain`.

### 1.4 Full `xtool.yml` schema (the `PackSchemaBase` struct in [Sources/PackLib/PackSchema.swift](https://github.com/xtool-org/xtool/blob/main/Sources/PackLib/PackSchema.swift))

| Key | Type | Required | Semantics (from Planner/Packer source) |
|---|---|---|---|
| `version` | Int | **yes** | Must be `1`. |
| `bundleID` | String | one of bundleID/orgID | The app bundle ID. Takes precedence over `orgID`. |
| `orgID` | String | one of bundleID/orgID | The bundle ID becomes `<orgID>.<product>`. |
| `product` | String | if the package has more than one library product | Name of the library product that is the app. |
| `infoPath` | String | no | Plist **shallow-merged** over the generated Info.plist; your keys win. |
| `entitlementsPath` | String | no | Entitlements plist. At build time xtool ad-hoc signs the bundle with these entitlements embedded. At install time the signer reads them back from the binary. |
| `iconPath` | String | no | Must end in `.png` (validated); 1024×1024 is recommended. Copied to the bundle root and `CFBundleIconFile` is set. |
| `resources` | [String] | no | Files or directories copied to the **root** of the `.app`. |
| `extensions` | [Extension] | no | Each entry has `product` (req), `infoPath` (req), `bundleID` (default `<appBundleID>.<product>`), `resources`, `entitlementsPath`. These are app extensions (`.appex` in `PlugIns/`). |
| `skipLSP` | Bool | no | Don't create the LSP/BSP helper file. |

**Unknown or misspelled keys are silently ignored** by the YAML decoder ([#353](https://github.com/xtool-org/xtool/issues/353)), e.g. `entitlements:` instead of `entitlementsPath:`. Double-check your spelling.

Not supported yet: an `assetCatalogs:` key. Asset-catalog compilation is in open PR [#325](https://github.com/xtool-org/xtool/issues/325). A `.xcassets` folder inside a SwiftPM target makes Swift Build run `actool`. On macOS that works, but on Linux/WSL it fails ([#310](https://github.com/xtool-org/xtool/issues/310)). Avoid `.xcassets` if you want builds to behave the same on Linux and macOS.

### 1.5 Info.plist handling ([Planner.swift](https://github.com/xtool-org/xtool/blob/main/Sources/PackLib/Planner.swift) + Packer.swift)

**Generated keys for the app**, which your `infoPath` plist can override:

* `CFBundleInfoDictionaryVersion=6.0`
* `CFBundleDevelopmentRegion=en`
* `CFBundleVersion="1"`
* `CFBundleShortVersionString="1.0.0"`
* `MinimumOSVersion=<iOS platform version from Package.swift>` (falls back to `13.0`)
* `CFBundleIdentifier`
* `CFBundleName`
* `CFBundleExecutable`
* `CFBundleDisplayName=<product>`. The docs page still says it isn't set; that's stale.
* `CFBundlePackageType=APPL`
* `UIDeviceFamily=[1,2]`
* `UISupportedInterfaceOrientations=[Portrait]`
* `UISupportedInterfaceOrientations~ipad=[all four]`
* `UILaunchScreen={}`

**Merge rule:** `infoPlist.merge(custom) { $1 }`. This is top-level only, and the custom value replaces the whole key (arrays and dicts are not deep-merged).

**Forced after the merge** (you can't override these):

* `UIRequiredDeviceCapabilities=["arm64"]`
* `LSRequiresIPhoneOS=true`
* `CFBundleSupportedPlatforms=["iPhoneOS"]` (`iPhoneSimulator` for simulator triples)
* `CFBundleIconFile=<icon basename>` when `iconPath` is set

**Not generated:**

* `UIApplicationSceneManifest`. You must add it; see §1.6.
* Xcode's `DT*` keys (`DTSDKName`, `DTXcode`, …). PR [#398](https://github.com/xtool-org/xtool/issues/398) is open. They only matter for App Store uploads, not sideloading.

**Minimum iOS:** taken from the `.iOS(...)` entry in `Package.swift` `platforms`. The same version is used for the generated builder package and for `MinimumOSVersion`.

**Don't put `Info.plist` inside `Sources/<Target>/`.** SwiftPM rejects a top-level `Info.plist` resource in a target. Keep it in e.g. `Resources/Info.plist` and point `infoPath` at it.

### 1.6 UIKit / app-delegate apps

**Supported.** Any `@main` entry point in the app library target works, because the executable gets `main` from it.

* A real-world xtool project uses `@main final class AppDelegate: UIResponder, UIApplicationDelegate` in its library target: [slopus/happy happy-ios](https://github.com/slopus/happy/blob/main/packages/happy-ios/Sources/HappyApp/AppDelegate.swift), with [xtool.yml](https://github.com/slopus/happy/blob/main/packages/happy-ios/xtool.yml).
* Some third-party templates claim xtool "expects a SwiftUI App". That isn't true per the source.

**UIScene is mandatory with the iOS 27 SDK.** Apple's TN3187 says: "In the next major release following iOS 26, UIScene lifecycle will be required when building with the latest SDK; otherwise, your app won't launch." ([TN3187](https://developer.apple.com/documentation/technotes/tn3187-migrating-to-the-uikit-scene-based-life-cycle)) To satisfy it:

* Add `UIApplicationSceneManifest` with a configuration.
* Implement `application(_:configurationForConnecting:options:)`.
* `$(PRODUCT_MODULE_NAME)` is an Xcode build-setting variable and **xtool doesn't expand it**. Write the literal `ModuleName.SceneDelegate`, or also set `delegateClass` in code.

**Caveat on the recorded SDK version** (`LC_BUILD_VERSION.sdk`):

* On Linux, xtool-built binaries record the *deployment target* as the SDK version ([#351](https://github.com/xtool-org/xtool/issues/351); fixes [#321](https://github.com/xtool-org/xtool/issues/321)/[#392](https://github.com/xtool-org/xtool/issues/392) still open). iOS then runs the app in compatibility mode: no Liquid Glass, old-SDK behaviours, and no UIScene enforcement.
* The xtool PRs say macOS isn't affected.
* Two macOS-CI projects disagree and work around it on macOS too:
  * [MicYou-iOS Package.swift](https://github.com/MicYou-Dev/MicYou-iOS/blob/swiftui/Package.swift) passes `-Xlinker -platform_version ios <min> <sdk>`.
  * [NOVA-ios](https://github.com/federicotaranudev/NOVA-ios/blob/main/.github/workflows/build-ipa.yml) re-links with `swiftc`.
* So the proposed Package.swift adds that linker flag when CI sets `APP_IOS_SDK_VERSION`, and the workflow prints `vtool -show-build` to check. Adopt the scene life cycle regardless, so the app is correct in both cases.

---

## 2. Unsigned CI builds

### 2.1 Can xtool build unsigned without an Apple account? **Yes.**

* In `DevBuildCommand`, auth is only read `if sign` (`-s/--sign`). Without it the flow is: `PackOperation` → build → pack → optional ad-hoc signing (only if `entitlementsPath` is set) → optional `--ipa` zip.
* On macOS, `EnsureSDKOperation` is a no-op (`#if os(macOS)`).
* Maintainer: "You can do this with `xtool dev build --ipa`!" ([#194](https://github.com/xtool-org/xtool/issues/194)).
* xtool's own integration test does `xtool new --skip-setup MyApp; xtool dev build` with no login ([smoke.bats](https://github.com/xtool-org/xtool/blob/main/integration-tests/suite/smoke.bats)).

What the resulting IPA contains:

* The `Payload/<App>.app` layout.
* The app is unsigned, or ad-hoc signed with your entitlements if `entitlementsPath` is set. Embedding them is useful because xtool, SideStore and AltStore read entitlements from the binary when they re-sign.
* The archive is a *stored* (uncompressed) zip; PR [#396](https://github.com/xtool-org/xtool/issues/396) is open. Recompress it in CI if size matters.

### 2.2 Real-world GitHub Actions evidence (run status checked via the API on 2026-10-30)

| Repo | Runner | How xtool is installed | Status |
|---|---|---|---|
| [simon01920/tabula-app-ios](https://github.com/simon01920/tabula-app-ios/blob/main/.github/workflows/ios.yml) | `xcode-27` (Swift 6.4, swiftbuild) | pinned `xtool.app.zip` + SHA-256, `Resources/bin` on PATH | success 2026-10-29. The log shows "Build complete! (49.78 sec) … Wrote to …/xtool/TabulaApp.app". |
| [MicYou-Dev/MicYou-iOS](https://github.com/MicYou-Dev/MicYou-iOS/blob/swiftui/.github/actions/setup-xtool/action.yml) | `macos-latest` | pinned zip + SHA-256 + XTL_CLI wrapper + `.build` cache | success 2026-10-22 |
| [CaramelKat/Tanukis-Stash](https://github.com/CaramelKat/Tanukis-Stash/blob/main/.github/workflows/release.yml) | `macos-15` | `brew install --cask xtool-org/tap/xtool`; `xtool dev build --configuration release --ipa` | success 2026-10-20 |
| [devinprater/iTruVoice](https://github.com/devinprater/iTruVoice/blob/main/.github/workflows/build.yml) | `macos-15` | brew | success 2026-10-27 |
| [hotwheelz3000/earmilk-app](https://github.com/hotwheelz3000/earmilk-app/blob/main/.github/workflows/build-ipa.yml) | `macos-latest` | brew; `xtool dev build --ipa` | success 2026-10-26 |
| [AhmedHeshamEG/teleprompter-studio](https://github.com/AhmedHeshamEG/teleprompter-studio/blob/master/.github/workflows/build-ipa.yml) | `macos-26` | brew; `xtool dev build --ipa` | success 2026-09-28 |

xtool's own CI uses the GitHub-hosted `xcode-27` label and `sudo xcode-select -s /Applications/Xcode_27.1.0.app` ([build.yml](https://github.com/xtool-org/xtool/blob/main/.github/workflows/build.yml), [configure-xcode](https://github.com/xtool-org/xtool/blob/main/.github/actions/configure-xcode/action.yml)). It doesn't run `xtool dev build` on macOS in CI; its Linux integration tests run on a self-hosted runner.

### 2.3 Runner images, Xcode and Swift (from [runner-images README](https://github.com/actions/runner-images), [xcode-27 readme](https://github.com/actions/runner-images/blob/main/images/macos/xcode-27-arm64-Readme.md), [macos-26 readme](https://github.com/actions/runner-images/blob/main/images/macos/macos-26-arm64-Readme.md), [Apple Xcode support table](https://developer.apple.com/support/xcode/))

| Label | OS / image | Xcodes | Default | Swift | iOS SDK |
|---|---|---|---|---|---|
| **`xcode-27`** (preview, arm64; [announcement #14404](https://github.com/actions/runner-images/issues/14404)) | macOS 27.0.1, image 20261026.0314.1 | 27.2 RC, **27.1**, 27.0 | 27.1 (`/Applications/Xcode_27.1.app`) | **6.4** | 27.1 |
| `macos-latest` = `macos-26` (GA, arm64) | macOS 26.6.2, image 20261013 | 26.0.1 … 26.6 | 26.6 | 6.3 | 26.5 |
| `macos-15` | macOS 15 | 16.x / 26.x | — | — | — |
| `macos-14` | deprecated, fully unsupported from 2026-11-02 | | | | |

Xcode 27.x requires macOS Tahoe 26.6+, deploys to iOS 15–27, and runs on Apple-silicon Macs only. Standard runners are free for public repositories. For private repositories, standard macOS minutes cost about $0.062/min versus $0.006/min on Linux ([pricing](https://docs.github.com/en/billing/reference/actions-runner-pricing)).

### 2.4 Fallbacks if xtool itself can't run in CI (same package + same xtool.yml)

1. **`xtool dev generate-xcode-project` + `xcodebuild`.** This is the most faithful fallback because it uses the same plan and xtool.yml. One quirk: the Xcode project sets `CFBundleDisplayName` to the product name. The array-ai/fieldnotes `app.yml` workflow (see §2.2 search) builds this way on `xcode-27`.
   ```bash
   xtool dev generate-xcode-project
   xcodebuild build -workspace xtool/SidecarPad.xcworkspace -scheme SidecarPad-App \
     -destination 'generic/platform=iOS' -configuration Release -derivedDataPath build/DD CODE_SIGNING_ALLOWED=NO
   mkdir -p Payload && cp -R build/DD/Build/Products/Release-iphoneos/SidecarPad-App.app Payload/
   zip -qry SidecarPad.ipa Payload
   ```
   The bundle's executable is named `SidecarPad-App`. This path also records the correct SDK version and compiles `.xcassets` with the real actool.
2. **Plain SwiftPM.** `env -u SDKROOT "$(xcrun -f swift)" build --swift-sdk arm64-apple-ios` (Swift 6.1–6.3) or `--build-system swiftbuild --triple arm64-apple-ios` (6.4). This only compiles the library. You would have to re-create xtool's builder package and Info.plist by hand, so it isn't recommended.
3. **`xcodebuild -scheme <lib> -destination generic/platform=iOS`** on the raw package. This builds a library only and produces no `.app`, so it's useful as a compile check only.

---

## 3. Signing and installing on Arch Linux with xtool

### 3.1 Requirements

* `sudo pacman -S usbmuxd libimobiledevice` (Arch `extra` has usbmuxd 1.1.1). udev normally starts `usbmuxd` when the iPad is plugged in; if not, run `sudo systemctl start usbmuxd`.
* The xtool AppImage. AppImages need FUSE. If it won't mount, install `fuse2` (or `fuse3`) or run with `APPIMAGE_EXTRACT_AND_RUN=1`. **Not verified on Arch.**
* `xtool auth login --mode password`. You enter your Apple ID, your **regular password** and a **2FA code**. 1.23.0 added a fallback from SMS 2FA to a trusted-device code. xtool emulates a Mac using Anisette data; the docs say credentials only go to Apple and recommend a secondary Apple ID.
  * App-specific passwords: an issue reporter tried one and got the same error ([#329](https://github.com/xtool-org/xtool/issues/329)). The docs say "email+password+2FA".
  * **Use ≥ 1.22.2.** Login error -80045 was fixed there, and 1.24.0 fixes another HTTP 503 auth failure. 1.22.2 and 1.23.0 each forced a re-login.
* **Darwin SDK / Xcode.xip: not needed to sign or install a prebuilt IPA.**
  * `InstallCommand` only loads the saved auth token and connects to the device. It never calls `EnsureSDKOperation`.
  * Signing uses xtool's built-in signer (zsign), not Apple tools.
  * Only `xtool setup`, `xtool new` (without `--skip-setup`) and `xtool dev ...` need the SDK.
* On the iPad, once:
  * Tap **Trust** this computer when pairing.
  * Enable **Developer Mode** (Settings › Privacy & Security), which requires a reboot.
  * After the first install, trust the developer certificate in Settings › General › VPN & Device Management ([first-app tutorial](https://xtool.sh/tutorials/xtooldocs/first-app)).

### 3.2 What `xtool install path.ipa` does ([IntegratedInstaller.swift](https://github.com/xtool-org/xtool/blob/main/Sources/XKit/Integration/IntegratedInstaller.swift), [AddAppOperation](https://github.com/xtool-org/xtool/blob/main/Sources/XKit/DeveloperServices/App%20IDs/DeveloperServicesAddAppOperation.swift))

1. Unzips the IPA, or accepts a `.app` directly.
2. Pairs with the device over lockdown.
3. Registers or reuses the App ID **`XTL-<8 hex chars of your identity>.<bundleID>`**. The prefix avoids collisions between free accounts. Re-installing the same bundle ID reuses the App ID, so it doesn't use up any of the 10-per-week budget.
4. Fetches a **new 7-day profile**.
5. Re-signs with get-task-allow and your team's entitlements. It reads the entitlements from the binary. On free teams it **keeps only entitlements xtool knows to be free**, plus `keychain-access-groups`, application-identifier, team-id and get-task-allow. It **drops unknown ones**.
6. Installs over the existing app. App data is kept because the bundle ID and team are unchanged.

**Certificates:** xtool stores its own development certificate and private key and reuses them. On a free team, if it has no stored cert that's still valid, it **revokes all existing development certificates**. It asks "Continue?" first ([FetchCertificateOperation](https://github.com/xtool-org/xtool/blob/main/Sources/XKit/DeveloperServices/Certificates/DeveloperServicesFetchCertificateOperation.swift)). SideStore and iloader do the same. **Don't use xtool and SideStore on the same Apple ID**, or use two Apple IDs. Otherwise each tool keeps revoking the other's cert, and apps signed with a revoked cert may stop launching.

### 3.3 One-command weekly re-sign and reinstall (`scripts/resign-ipad.sh`)

```bash
#!/usr/bin/env bash
# Re-sign + reinstall the latest CI-built IPA with a free Apple ID (7-day profile). Run at least weekly.
set -euo pipefail
REPO="${REPO:-OWNER/REPO}"            # GitHub repo with the workflow below
APP="${APP:-SidecarPad}"              # == product: in xtool.yml
WORKFLOW="${WORKFLOW:-build-ipa.yml}"
UDID="${UDID:-}"                      # optional; find with: xtool devices
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT

# 1. usbmuxd must be running (udev usually starts it when the iPad is plugged in)
systemctl is-active --quiet usbmuxd || sudo systemctl start usbmuxd

# 2. Apple ID session (interactive only when the token is missing/invalid: password + 2FA)
if ! xtool auth status | grep -q '^Logged in'; then
  xtool auth login --mode password
fi

# 3. Fetch the IPA: rolling "nightly" release asset (no GitHub auth needed for public repos) ...
if ! curl -fsSL -o "$work/$APP.ipa" "https://github.com/$REPO/releases/download/nightly/$APP.ipa"; then
  # ... or the newest successful run's artifact (needs `gh auth login`)
  run_id="$(gh run list -R "$REPO" -w "$WORKFLOW" -b main -s success -L 1 --json databaseId -q '.[0].databaseId')"
  gh run download "$run_id" -R "$REPO" -n "$APP-ipa" -D "$work"
fi

# 4. Provision (new 7-day profile), re-sign, install over the existing app (data preserved)
xtool install ${UDID:+--udid "$UDID"} --usb "$work/$APP.ipa"
```

Notes:

* If `xtool auth status` shows an expired token, or install fails with an auth error, run `xtool auth login` again.
* Run the script when the iPad is plugged in and unlocked.
* You can schedule it with a systemd user timer, but 2FA and the USB connection make fully unattended runs unreliable.

**Option (c), build on Arch instead of CI:**

1. Install Swift 6.4.
2. Download Xcode 27.1 `.xip`. A free Apple ID can download it from developer.apple.com/download/all.
3. `xtool sdk install ~/Downloads/Xcode_27.1.xip`.
4. `xtool dev`.

This works, but Linux builds currently record the wrong SDK version (§1.6), and `.xcassets` doesn't work on Linux.

---

## 4. Free Apple ID (personal team) limits

**Official limits** from Apple's [Choosing a Membership](https://developer.apple.com/support/compare-memberships/) page, quoted:

* "You can register up to **10 App IDs, which expire after 7 days**."
* "You can register up to **3 devices**, which expire after 7 days."
* "You can install up to **3 apps per device**. Provisioning profiles … will **expire 7 days from issuance**."

Each app extension needs its own App ID; SideStore has an option to reuse the main profile for extensions. **Keep the app extension-free.**

Per Apple's [Supported capabilities (iOS)](https://developer.apple.com/help/account/reference/supported-capabilities-ios), the "Apple Developer" (free) column only covers:

* **App Groups**
* **Background modes**
* **Data protection**
* **HealthKit**
* **HomeKit**
* **Inter-App Audio**
* **Keychain sharing**
* **Maps**
* **Wireless Accessory Configuration**

**Not available free** (selection): Push Notifications, all iCloud capabilities, Associated Domains, Network Extensions, Personal VPN, Access Wi-Fi Information, App Attest, Sign in with Apple, Siri, Apple Pay, Wallet, Game Center, In-App Purchase, NFC, Multipath, Time Sensitive / Communication Notifications, Extended Virtual Addressing, Increased *Debugging* Memory Limit, WeatherKit, Hotspot, Push to Talk, Family Controls.

**Multicast networking** (`com.apple.developer.networking.multicast`) isn't in the table at all. It is a managed capability that you request from Apple, so it's paid-account only in practice.

How the tools handle these:

* **xtool:**
  * Treats **App Groups as not free** (a `FIXME` in [DeveloperServicesCapability.swift](https://github.com/xtool-org/xtool/blob/main/Sources/XKit/DeveloperServices/App%20IDs/Entitlements/DeveloperServicesCapability.swift)) and strips them on free teams, even though Apple allows them.
  * Strips every entitlement it doesn't recognise, including `com.apple.developer.kernel.increased-memory-limit`.
* **SideStore:** currently drops increased-memory-limit too ([SideStore#1616](https://github.com/SideStore/SideStore/issues/1616)), although Xcode personal teams and AltStore 2.3 can get it on free accounts.

**Your required features need no restricted entitlement:**

| Feature | What it needs | Free? |
|---|---|---|
| Bonjour advertising via `NWListener` (fixed service type, e.g. `_sidecar._tcp`) | Info.plist `NSBonjourServices` + `NSLocalNetworkUsageDescription`; triggers the Local Network prompt | ✅. TN3179: multicast entitlement only for sending/receiving UDP multicast/broadcast, "arbitrary Bonjour service types" or "browsing for all advertised service types" ([TN3179](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)). Incoming TCP accept doesn't need local-network access. |
| Keychain (own items) | nothing (default access group = app ID) | ✅ (Keychain sharing is also free) |
| Background modes | `UIBackgroundModes` in Info.plist | ✅ (normal iOS background limits apply; a listener is suspended when the app is backgrounded) |
| `UIApplicationSupportsMultipleScenes` | Info.plist only | ✅ |
| GameController | framework; optional `GCSupportedGameControllers`/`GCSupportsControllerUserInteraction` plist keys | ✅ |
| Haptics (`UIFeedbackGenerator`, `UICanvasFeedbackGenerator`, Core Haptics) | nothing | ✅ |
| Apple Pencil Pro (squeeze `UIPencilInteraction.Squeeze`, `UIPencilHoverPose`, `UITouch.rollAngle`, `UICanvasFeedbackGenerator`) | nothing; **iPadOS 17.5+** per Apple docs metadata; `PKToolPickerCustomItem` is 18.0 | ✅ |

---

## 5. SideStore

### 5.1 Current state (from [SideStore releases](https://github.com/SideStore/SideStore/releases) and [docs](https://docs.sidestore.io))

**Versions:**

* **0.7.0-alpha** (2026-09-15) "fixes for 503 during sign-in". 0.6.4 and older can't sign in after an Apple server change.
* 0.6.3 (2026-05) switched to **LocalDevVPN** and says "Please reinstall with iloader if on iOS 26.4+".
* Installer: **iloader** v2.4.1 (2026-10-28).

**Install procedure** ([prerequisites](https://docs.sidestore.io/docs/installation/prerequisites), [install](https://docs.sidestore.io/docs/installation/install)):

1. On the iPad, install **LocalDevVPN** from the App Store (id6755608044) and connect it once.
2. On Arch, install `usbmuxd` and iloader. Use the AppImage/deb/rpm from [nab138/iloader](https://github.com/nab138/iloader); `iloader-bin` on the AUR (2.4.1) is community-maintained.
3. Plug in the iPad and trust the computer. In iloader, sign in with your Apple ID, pick the device and choose **Install SideStore (Stable)**, or **LiveContainer+SideStore**. iloader also places the pairing file.
4. On the iPad:
   1. Settings › General › VPN & Device Management › trust ("Allow & Restart" on 18+).
   2. Enable Developer Mode.
   3. Connect LocalDevVPN.
   4. Open SideStore and sign in.
   5. In My Apps, tap the **"7 DAYS"** pill to refresh SideStore itself. Accept the revoke/new-certificate prompt.

**Pairing file:**

* It may expire after iPadOS updates or resets, "also at random times".
* To fix it, re-place it with iloader (Delete Stored Pairing → Trust → Manage Pairing File → Place) ([pairing-file](https://docs.sidestore.io/docs/advanced/pairing-file)).
* So you still occasionally need the Linux box.

**Legacy and alternative tools** ([alternative.mdx](https://docs.sidestore.io/docs/advanced/alternative)):

* StosVPN and WireGuard are superseded by LocalDevVPN.
* JitterbugPair and idevice_pair are older pairing tools; iloader replaces them.
* **AltServer-Linux / Altcon** (`docker run … ghcr.io/sidestore/altcon`) is listed under "Alternative/Outdated".
* [Impactor](https://github.com/khcrysalis/Impactor) v2.7.2 is another cross-platform sideloader, with a Linux AppImage and the `plumesign` CLI.

**Unsigned IPAs:** accepted. SideStore re-signs everything with your certificate. The FAQ says "Your apps shouldn't need any modification to work with SideStore". Projects ship unsigned xtool IPAs for "AltStore, SideStore, Sideloadly or xtool" ([MicYou README](https://github.com/MicYou-Dev/MicYou-iOS)). SideStore installs under its own bundle-ID scheme, so it doesn't share an App ID or install with xtool's `XTL-…` copy. iloader can also "Import any IPA" directly.

**Refresh:**

* SideStore re-signs on the device: it renews the profiles and reinstalls using the pairing file plus the VPN loopback.
* It refreshes in the background, through a Shortcuts automation, or when you tap "Refresh All". **LocalDevVPN must be connected.**
* 0.6.4/0.7 added a "cellular refresh" option via Shortcuts.
* If SideStore itself expires, you need the computer (iloader) to reinstall it.

**Slots:**

* "SideStore can only install 3 apps (including itself) at a time… only 10 different apps may be installed in a week" ([FAQ](https://docs.sidestore.io/docs/faq)).
* So SideStore plus your app uses 2 of 3 slots.

**LiveContainer:**

* [LiveContainer](https://github.com/LiveContainer/LiveContainer) 3.8.0 (nightly 2026-10). The **LiveContainer+SideStore** build uses **one** slot and runs any number of apps inside it.
* **Not recommended for this app:**
  * Guest apps run under LiveContainer's Info.plist, so your `NSBonjourServices` isn't honoured. Local network access is attributed to LiveContainer. MicYou documents exactly this: "LiveContainer doesn't declare MicYou's Bonjour service, the system blocks LAN discovery."
  * The scene manifest, multiple scenes and similar settings are controlled by the host app.
  * Install the app natively instead.

### 5.2 xtool vs SideStore for this project

| | xtool on Arch (a) | SideStore (b) |
|---|---|---|
| Weekly refresh | Run `resign-ipad.sh` with the iPad plugged in (USB) | Automatic on the iPad (needs LocalDevVPN on; Shortcut automation) |
| Computer needed | Every ≤7 days | Initial install + after a pairing-file loss / SideStore expiry |
| Slots used | 1 (your app) | 2 (SideStore + app); 1 with LC+SideStore, but LC breaks Bonjour |
| Getting new builds | Script pulls the CI IPA | Download the IPA in Safari (release asset) → open in SideStore, or add an [AltStore-format source](https://faq.altstore.io/developers/make-a-source) for one-tap updates |
| Entitlements on free team | xtool's free list; strips App Groups and unknown ones | SideStore's list; app groups rewritten; strips increased-memory-limit |
| Fragility | Apple auth changes (fixed in 1.22.2/1.24.0); certificate revocation if mixed with other tools | Same auth churn (0.6.4 broken → 0.7.0-alpha); VPN/pairing issues after iPadOS updates |
| Trust prompts | Developer Mode + trust certificate once | Same |

**Recommendation:** use SideStore for day-to-day life, since it refreshes untethered. Keep xtool for fast dev loops with `xtool dev`, either on a second Apple ID or by not using SideStore with the same ID. Never run both on one Apple ID; they revoke each other's certificates.

---

## 6. Proposed files

Placeholders: module/product **`SidecarPad`**, bundle ID **`io.github.CHANGEME.SidecarPad`**, Bonjour type **`_sidecar._tcp`**. Rename consistently: `Package.swift` name/product/target, `product:` in xtool.yml, `UISceneDelegateClassName`, `APP_NAME` in the workflow, and `APP` in the script.

### 6.1 Layout

```
.
├── Package.swift
├── xtool.yml
├── Resources/
│   ├── Info.plist          # merged by xtool (outside Sources/ on purpose)
│   └── AppIcon.png         # 1024x1024 PNG
├── Sources/SidecarPad/
│   ├── AppDelegate.swift
│   ├── SceneDelegate.swift
│   └── RootViewController.swift
├── scripts/resign-ipad.sh  # §3.3
├── .github/workflows/build-ipa.yml
└── .gitignore              # add: /.build /xtool /dist .bsp/ (or set skipLSP: true)
```

### 6.2 `Package.swift`

Minimum iOS: **17.5** is recommended. It's the first release with the Pencil Pro APIs (squeeze, barrel roll, `UICanvasFeedbackGenerator`), and every Pencil-Pro-capable iPad runs 17.5 or later. If you want iOS 26 UIKit APIs (Liquid Glass, new menu/window APIs) without `#available` checks, raise it to `"26.0"`. Use the string form, because `.v26` needs tools-version 6.2.

```swift
// swift-tools-version: 6.0
import PackageDescription

let minimumIOS = "17.5"

// xtool links the app without telling the linker the real SDK version, so LC_BUILD_VERSION may record
// sdk == minimumIOS and iPadOS runs the app in compatibility mode (xtool-org/xtool#351; MicYou-iOS does
// the same workaround on macOS CI). CI exports APP_IOS_SDK_VERSION=$(xcrun --sdk iphoneos --show-sdk-version).
// Unset (e.g. local builds) => no extra flags.
let sdkVersionLinkerFlags: [LinkerSetting] = Context.environment["APP_IOS_SDK_VERSION"].map { sdk in
    [.unsafeFlags(["-Xlinker", "-platform_version", "-Xlinker", "ios", "-Xlinker", minimumIOS, "-Xlinker", sdk])]
} ?? []

let package = Package(
    name: "SidecarPad",
    platforms: [
        .iOS(minimumIOS),   // -> MinimumOSVersion in the generated Info.plist
        .macOS(.v14),       // kept from the `xtool new` template (host tooling / LSP)
    ],
    products: [
        // xtool: exactly one library product = the app (or name it via `product:` in xtool.yml)
        .library(name: "SidecarPad", targets: ["SidecarPad"]),
    ],
    targets: [
        .target(
            name: "SidecarPad",
            linkerSettings: sdkVersionLinkerFlags
        ),
    ]
)
```

### 6.3 `xtool.yml`

```yaml
version: 1
bundleID: io.github.CHANGEME.SidecarPad   # xtool signs it as XTL-xxxxxxxx.<this>; don't change it often (10 App IDs / 7 days)
product: SidecarPad
infoPath: Resources/Info.plist
iconPath: Resources/AppIcon.png
skipLSP: true
# entitlementsPath: Resources/SidecarPad.entitlements   # only if ever needed; free-team-safe keys only
# resources:                                            # files copied to the .app root
#   - Resources/SomeFile.json
```

`iconPath` is the documented approach. **UNVERIFIED** that a loose `CFBundleIconFile` PNG renders on iPadOS 27. If it doesn't, add `CFBundleIcons~ipad` / `CFBundleIconFiles` entries and the sized PNGs through `resources:`, or wait for asset catalog support ([#325](https://github.com/xtool-org/xtool/issues/325)).

### 6.4 `Resources/Info.plist` (merged over xtool's defaults)

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDisplayName</key>
    <string>Sidecar</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <!-- iPad only (xtool default is [1,2]) -->
    <key>UIDeviceFamily</key>
    <array><integer>2</integer></array>
    <!-- Required for apps built with the iOS 27 SDK (TN3187). xtool does not expand $(PRODUCT_MODULE_NAME). -->
    <key>UIApplicationSceneManifest</key>
    <dict>
        <key>UIApplicationSupportsMultipleScenes</key>
        <false/>
        <key>UISceneConfigurations</key>
        <dict>
            <key>UIWindowSceneSessionRoleApplication</key>
            <array>
                <dict>
                    <key>UISceneConfigurationName</key>
                    <string>Default Configuration</string>
                    <key>UISceneDelegateClassName</key>
                    <string>SidecarPad.SceneDelegate</string>
                </dict>
            </array>
        </dict>
    </dict>
    <key>UILaunchScreen</key>
    <dict/>
    <key>UIApplicationSupportsIndirectInputEvents</key>
    <true/>
    <!-- Bonjour advertising via NWListener: no multicast entitlement needed (TN3179) -->
    <key>NSLocalNetworkUsageDescription</key>
    <string>Sidecar uses your local network to connect to your Linux computer.</string>
    <key>NSBonjourServices</key>
    <array>
        <string>_sidecar._tcp</string>
    </array>
</dict>
</plist>
```

### 6.5 Minimal UIKit sources

```swift
// Sources/SidecarPad/AppDelegate.swift
import UIKit

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        true
    }

    func application(_ application: UIApplication,
                     configurationForConnecting connectingSceneSession: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let config = UISceneConfiguration(name: "Default Configuration", sessionRole: connectingSceneSession.role)
        config.delegateClass = SceneDelegate.self   // belt and braces vs. the plist class name
        return config
    }
}
```

```swift
// Sources/SidecarPad/SceneDelegate.swift
import UIKit

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(_ scene: UIScene, willConnectTo session: UISceneSession,
               options connectionOptions: UIScene.ConnectionOptions) {
        guard let windowScene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: windowScene)
        window.rootViewController = RootViewController()
        window.makeKeyAndVisible()
        self.window = window
    }
}
```

```swift
// Sources/SidecarPad/RootViewController.swift
import UIKit

final class RootViewController: UIViewController {
    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .systemBackground
    }
}
```

### 6.6 `.github/workflows/build-ipa.yml`

```yaml
name: Build unsigned IPA

on:
  push:
    branches: ["**"]
    tags: ["v*"]
  pull_request:
  workflow_dispatch:

permissions:
  contents: read

concurrency:
  group: build-ipa-${{ github.ref }}
  cancel-in-progress: true

env:
  APP_NAME: SidecarPad   # must equal `product:` in xtool.yml
  XTOOL_VERSION: "1.24.0"
  XTOOL_SHA256: a56e98d69dea02a97a17e609f0ae8771c57ffaab23f1d62513fd9ada222026b3
  XCODE_APP: /Applications/Xcode_27.1.app
  MIN_SDK_MAJOR: "26"    # warn if the binary records an older SDK (compatibility mode)

jobs:
  ipa:
    name: Unsigned .ipa (xtool)
    # Preview image: macOS 27 + Xcode 27.1 (Swift 6.4, iOS 27.1 SDK). GA fallback: `macos-26` with
    # XCODE_APP=/Applications/Xcode_26.6.app (Swift 6.3, iOS 26.5 SDK).
    runs-on: xcode-27
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@v7

      - name: Select Xcode
        run: |
          set -euo pipefail
          sudo xcode-select -s "$XCODE_APP"
          xcodebuild -version
          swift --version
          SDK="$(xcrun --sdk iphoneos --show-sdk-version)"
          echo "iOS SDK: $SDK"
          echo "APP_IOS_SDK_VERSION=$SDK" >> "$GITHUB_ENV"   # read by Package.swift

      - name: Install xtool (pinned, SHA-256 verified)
        run: |
          set -euo pipefail
          curl -fsSL --retry 3 -o "$RUNNER_TEMP/xtool.app.zip" \
            "https://github.com/xtool-org/xtool/releases/download/${XTOOL_VERSION}/xtool.app.zip"
          echo "${XTOOL_SHA256}  $RUNNER_TEMP/xtool.app.zip" | shasum -a 256 -c -
          ditto -x -k "$RUNNER_TEMP/xtool.app.zip" "$RUNNER_TEMP"
          # Use the CLI wrapper (sets XTL_CLI=1). Contents/MacOS/xtool alone starts the GUI and hangs.
          echo "$RUNNER_TEMP/xtool.app/Contents/Resources/bin" >> "$GITHUB_PATH"

      - name: xtool smoke test (60 s cap)
        run: perl -e 'alarm shift; exec @ARGV' 60 xtool --version < /dev/null

      - name: Stamp version into Info.plist (CI copy only)
        run: |
          set -euo pipefail
          PLIST=Resources/Info.plist
          plutil -lint "$PLIST"
          if [[ "$GITHUB_REF" == refs/tags/v* ]]; then
            plutil -replace CFBundleShortVersionString -string "${GITHUB_REF_NAME#v}" "$PLIST"
          fi
          plutil -replace CFBundleVersion -string "$GITHUB_RUN_NUMBER" "$PLIST"

      - name: Build (no Apple ID, no signing)
        run: |
          set -euo pipefail
          perl -e 'alarm shift; exec @ARGV' 1500 xtool dev build --configuration release --ipa < /dev/null
          test -f "xtool/$APP_NAME.ipa"

      - name: Inspect and recompress
        run: |
          set -euo pipefail
          work="$RUNNER_TEMP/ipa"; rm -rf "$work"; mkdir -p "$work" dist
          unzip -q "xtool/$APP_NAME.ipa" -d "$work"
          app="$work/Payload/$APP_NAME.app"
          plutil -p "$app/Info.plist"
          /usr/libexec/PlistBuddy -c 'Print :UIApplicationSceneManifest' "$app/Info.plist"
          file "$app/$APP_NAME"
          xcrun vtool -show-build "$app/$APP_NAME"
          sdk="$(xcrun vtool -show-build "$app/$APP_NAME" | awk '$1=="sdk"{print $2; exit}')"
          if [ -n "$sdk" ] && [ "${sdk%%.*}" -lt "$MIN_SDK_MAJOR" ]; then
            echo "::warning::Binary records iOS SDK $sdk; iPadOS will run it in compatibility mode."
          fi
          codesign -dvv "$app" 2>&1 || echo "(no code signature - expected for an unsigned build)"
          # xtool writes a stored (uncompressed) zip; recompress for download size
          (cd "$work" && zip -qry -9 "$GITHUB_WORKSPACE/dist/$APP_NAME.ipa" Payload)
          (cd dist && shasum -a 256 "$APP_NAME.ipa" | tee SHA256SUMS.txt)

      - uses: actions/upload-artifact@v7
        with:
          name: ${{ env.APP_NAME }}-ipa
          path: |
            dist/${{ env.APP_NAME }}.ipa
            dist/SHA256SUMS.txt
          if-no-files-found: error
          retention-days: 30

  # Optional: a stable download URL for the iPad (SideStore) and the Arch script, without GitHub login.
  publish:
    needs: ipa
    if: github.event_name == 'push' && (github.ref == 'refs/heads/main' || startsWith(github.ref, 'refs/tags/v'))
    runs-on: ubuntu-latest
    permissions:
      contents: write
    env:
      GH_TOKEN: ${{ github.token }}
      GH_REPO: ${{ github.repository }}
    steps:
      - uses: actions/download-artifact@v8
        with:
          name: ${{ env.APP_NAME }}-ipa
          path: dist
      - name: Publish release
        run: |
          set -euo pipefail
          if [[ "$GITHUB_REF" == refs/tags/v* ]]; then
            gh release create "$GITHUB_REF_NAME" --verify-tag \
              --title "$APP_NAME $GITHUB_REF_NAME" \
              --notes "Unsigned IPA. Install with SideStore or 'xtool install'." dist/*
          else
            gh release delete nightly --yes --cleanup-tag || true
            gh release create nightly --prerelease --target "$GITHUB_SHA" \
              --title "Nightly ${GITHUB_SHA::7}" \
              --notes "Unsigned IPA from ${GITHUB_SHA}. Install with SideStore or 'xtool install'." dist/*
          fi
```

**Confidence:**

* **High** that the `ipa` job works as written.
  * The same install method (pinned zip + `Resources/bin`) and `xtool dev build` on the `xcode-27` runner are green in tabula-app-ios (xtool 1.20.1; 1.24.0 makes no build-path changes per its release notes).
  * The runner paths (`/Applications/Xcode_27.1.app`) and the SHA-256 were verified.
  * `vtool`, `PlistBuddy`, `zip` and `plutil` are stock macOS.
* **Medium** on the UIKit specifics until the first green run:
  * that `@main UIApplicationDelegate` links from the library target (happy-ios does this);
  * that the `-platform_version` flag yields `sdk 27.1` (MicYou does exactly this on macOS CI).
* **Medium** on the `publish` job: `gh release` delete/re-create of a moving tag is standard, but untested here.
* `xcode-27` is a preview image with possible queueing delays. If it's retired or renamed when Xcode 27 goes GA, switch the label to `macos-26`, or to its successor.

---

## 7. Gotchas checklist

1. **Use the CLI wrapper on macOS** (`Resources/bin/xtool`). Running `Contents/MacOS/xtool` without `XTL_CLI=1` hangs CI.
2. **Info.plist lives outside `Sources/<target>`.** Use `infoPath`.
3. **Add `UIApplicationSceneManifest` yourself** with a literal `Module.SceneDelegate`. iOS 27 SDK apps without it won't launch.
4. **No `.xcassets`** in targets if you also build on Linux; use `iconPath`.
5. **Don't change `bundleID` casually.** The free tier allows 10 App IDs per 7 days, and each install tool has its own prefix/suffix scheme.
6. **Never mix xtool and SideStore on one Apple ID** (certificate revocation).
7. **No app extensions.** Each costs an App ID and possibly a slot.
8. **Pin xtool ≥ 1.24.0 and SideStore ≥ 0.7.0-alpha.** Apple auth changes broke older versions in September/October 2026.
9. **LiveContainer breaks Bonjour** for guest apps; install natively.
10. **Typos in xtool.yml are silently ignored.** Check the built `xtool/<App>.app/Info.plist`, or the CI "Inspect" step output.
