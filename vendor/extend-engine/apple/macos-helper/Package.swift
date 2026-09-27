// swift-tools-version: 5.9
import PackageDescription

// The executable's name is what a Carbon sees for a helper the engine builds itself: the process in
// Activity Monitor and the entry under Accessibility and Screen Recording. The Mac app ships it
// renamed and signed as "Silicon Extend Helper". Target and module names nobody sees stay upstream's.
let package = Package(
  name: "silicon-extend-macos-helper",
  platforms: [.macOS(.v13)],
  products: [
    .executable(
      name: "silicon-extend-macos-helper",
      targets: ["AgentDeviceMacOSHelper"]
    ),
  ],
  targets: [
    .target(
      name: "AgentDeviceMacOSInput"
    ),
    .executableTarget(
      name: "AgentDeviceMacOSHelper",
      dependencies: ["AgentDeviceMacOSInput"]
    ),
    .testTarget(
      name: "AgentDeviceMacOSInputTests",
      dependencies: ["AgentDeviceMacOSInput"]
    ),
    .testTarget(
      name: "AgentDeviceMacOSHelperTests",
      dependencies: ["AgentDeviceMacOSHelper"]
    ),
  ]
)
