import CoreGraphics
import XCTest
@testable import AgentDeviceMacOSHelper

final class NativeRecordingSelectionTests: XCTestCase {
  func testAuxiliaryWindowsDoNotHideTheCaptureWindow() {
    let display = CGRect(x: 0, y: 0, width: 1920, height: 1080)
    let main = CGRect(x: 40, y: 80, width: 560, height: 330)
    for auxiliary in [CGRect.zero, CGRect(x: -200, y: -200, width: 1, height: 1)] {
      XCTAssertEqual(nativeRecordingDisplayIndex(windowFrames: [auxiliary, main], displayFrames: [display]), 0)
    }
    XCTAssertNil(nativeRecordingDisplayIndex(windowFrames: [.zero], displayFrames: [display]))
    XCTAssertNil(nativeRecordingDisplayIndex(windowFrames: [], displayFrames: [display]))
  }

  func testDisplayWithLargestWindowIntersectionWins() {
    let displays = [CGRect(x: 0, y: 0, width: 1920, height: 1080), CGRect(x: 1920, y: 0, width: 1920, height: 1080)]
    XCTAssertEqual(nativeRecordingDisplayIndex(windowFrames: [CGRect(x: 1900, y: 100, width: 600, height: 400)], displayFrames: displays), 1)
  }

  func testScreenRecordingRefusalNamesThePermissionAndAReason() {
    let declined = NSError(domain: "com.apple.ScreenCaptureKit.SCStreamErrorDomain", code: -3801)
    guard case .commandFailed(let message, let details)? = nativeRecordingStartError(declined) as? HelperError else {
      return XCTFail("a Screen Recording refusal must become a helper failure with a reason")
    }
    XCTAssertTrue(message.contains("Screen Recording"))
    XCTAssertEqual(details["reason"], "screen_recording_permission_denied")
    XCTAssertEqual(details["permission"], "screen-recording")
  }

  func testOtherStartErrorsPassThroughUnchanged() {
    let other = NSError(domain: "com.apple.ScreenCaptureKit.SCStreamErrorDomain", code: -3802)
    XCTAssertTrue((nativeRecordingStartError(other) as NSError) === other)
    let helper = HelperError.commandFailed("recording output already exists")
    guard case .commandFailed(let message, _)? = nativeRecordingStartError(helper) as? HelperError else {
      return XCTFail("a helper failure must pass through")
    }
    XCTAssertEqual(message, "recording output already exists")
  }
}
