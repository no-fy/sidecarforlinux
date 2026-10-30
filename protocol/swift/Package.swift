// swift-tools-version: 6.0
// Swift implementation of the sidecarforlinux wire protocol, shared by the
// iPad app and tested on Linux + macOS against protocol/vectors/vectors.json.
import PackageDescription

let package = Package(
    name: "SidecarProtocol",
    platforms: [.iOS("17.5"), .macOS(.v14)],
    products: [
        .library(name: "SidecarProtocol", targets: ["SidecarProtocol"]),
    ],
    dependencies: [
        .package(url: "https://github.com/apple/swift-crypto.git", "3.0.0"..<"6.0.0"),
    ],
    targets: [
        .target(
            name: "SidecarProtocol",
            dependencies: [.product(name: "Crypto", package: "swift-crypto")]
        ),
        .testTarget(name: "SidecarProtocolTests", dependencies: ["SidecarProtocol"]),
    ],
    swiftLanguageModes: [.v5]
)
