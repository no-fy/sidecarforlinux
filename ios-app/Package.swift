// swift-tools-version: 6.0
// iPad app for sidecarforlinux. Built with xtool (see xtool.yml); the app is
// the single library product below.
import PackageDescription

let minimumIOS = "17.5"  // first release with Pencil Pro APIs (roll, squeeze, canvas haptics)

// xtool may record the deployment target as the SDK version in LC_BUILD_VERSION
// (xtool-org/xtool#351), which makes iPadOS run the app in compatibility mode.
// CI exports APP_IOS_SDK_VERSION; local builds don't add the flag.
let sdkVersionLinkerFlags: [LinkerSetting] = Context.environment["APP_IOS_SDK_VERSION"].map { sdk in
    [.unsafeFlags(["-Xlinker", "-platform_version", "-Xlinker", "ios", "-Xlinker", minimumIOS, "-Xlinker", sdk])]
} ?? []

let package = Package(
    name: "SidecarPad",
    platforms: [
        .iOS(minimumIOS),
        .macOS(.v14),
    ],
    products: [
        .library(name: "SidecarPad", targets: ["SidecarPad"]),
    ],
    dependencies: [
        .package(path: "../protocol/swift"),
        .package(url: "https://github.com/apple/swift-certificates.git", from: "1.21.0"),
        // swift-crypto 4.x/5.x add a Linux-only CXKCPShims target with a resource
        // bundle; xtool 1.24.0's packer expects that bundle on iOS too and fails
        // ("swift-crypto_CXKCPShims.bundle couldn't be opened"). 3.x avoids it.
        .package(url: "https://github.com/apple/swift-crypto.git", "3.12.3"..<"4.0.0"),
        .package(url: "https://github.com/apple/swift-asn1.git", from: "1.1.0"),
    ],
    targets: [
        // The app (UIKit). Its sources are wrapped in `#if canImport(UIKit)`
        // so `swift test` can build the package on macOS.
        .target(
            name: "SidecarPad",
            dependencies: [
                "SidecarNet",
                .product(name: "SidecarProtocol", package: "swift"),
            ],
            linkerSettings: sdkVersionLinkerFlags
        ),
        // Networking, TLS identity, pairing state machine: no UIKit, so it
        // is tested on macOS in CI against Apple's real TLS stack.
        .target(
            name: "SidecarNet",
            dependencies: [
                .product(name: "SidecarProtocol", package: "swift"),
                .product(name: "X509", package: "swift-certificates"),
                .product(name: "Crypto", package: "swift-crypto"),
                .product(name: "SwiftASN1", package: "swift-asn1"),
            ]
        ),
        .testTarget(
            name: "SidecarNetTests",
            dependencies: ["SidecarNet", .product(name: "SidecarProtocol", package: "swift")]
        ),
    ],
    swiftLanguageModes: [.v5]
)
