import Foundation
import XCTest
@testable import AgentDeviceMacOSHelper

final class BoundApplicationTests: XCTestCase {
  func testAppSurfaceRequiresAnExplicitIdentity() {
    XCTAssertThrowsError(try resolveTargetApplication(bundleId: nil, surface: "app")) { error in
      guard case HelperError.invalidArgs(let message) = error else { return XCTFail("wrong error: \(error)") }
      XCTAssertTrue(message.contains("--bundle-id"))
    }
  }

  func testSnapshotDoesNotSubstituteFrontmostForMissingBoundApp() {
    let bundle = "com.teamofsilicons.bridge.nonexistent.\(UUID().uuidString)"
    XCTAssertThrowsError(try captureSnapshotResponse(surface: "app", bundleId: bundle)) { error in
      guard case HelperError.commandFailed(_, let details) = error else { return XCTFail("wrong error: \(error)") }
      XCTAssertEqual(details["bundleId"], bundle)
    }
  }
  func testAppBundlePathUsesMetadataAndRejectsNonApps() throws {
    let temporary = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    let app = temporary.appendingPathComponent("Outside Standard Locations.app")
    let contents = app.appendingPathComponent("Contents")
    try FileManager.default.createDirectory(at: contents, withIntermediateDirectories: true)
    defer { try? FileManager.default.removeItem(at: temporary) }
    let data = try PropertyListSerialization.data(fromPropertyList: ["CFBundleIdentifier": "com.example.MixedCase", "CFBundlePackageType": "APPL"], format: .xml, options: 0)
    try data.write(to: contents.appendingPathComponent("Info.plist"))
    XCTAssertEqual(try resolveAppBundleAtPath(app.path)["bundleId"], "com.example.MixedCase")
    XCTAssertThrowsError(try resolveAppBundleAtPath(temporary.path))
  }

  func testAppWindowSelectionIgnoresAuxiliaryAndOffscreenWindows() {
    let display = CGRect(x: 0, y: 0, width: 1920, height: 1080)
    let main = CGRect(x: 20, y: 30, width: 560, height: 330)
    let offscreen = CGRect(x: -1000, y: -1000, width: 100, height: 100)
    XCTAssertEqual(appCaptureWindowIndex(windowFrames: [.zero, offscreen, main], displayFrames: [display]), 2)
    XCTAssertNil(appCaptureWindowIndex(windowFrames: [.zero, offscreen], displayFrames: [display]))
  }

}
