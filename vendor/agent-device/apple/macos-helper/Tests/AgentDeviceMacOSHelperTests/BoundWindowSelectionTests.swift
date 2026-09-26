import CoreGraphics
import Foundation
import XCTest
@testable import AgentDeviceMacOSHelper

/// Pure selection rules shared by app screenshots and app-scoped recordings. No capture runs.
final class BoundWindowSelectionTests: XCTestCase {
  private let displays = [CGRect(x: 0, y: 0, width: 1920, height: 1080), CGRect(x: 1920, y: 0, width: 2560, height: 1440)]
  private let real = CGRect(x: 40, y: 80, width: 560, height: 330)

  func testRecordingDisplayFollowsTheRealWindowNotALargerBackingWindow() {
    // The app's real window is on display 1; an untitled, larger backing window sits on display 2.
    let candidates = [
      CaptureWindowCandidate(title: "Editor", frame: real, layer: 0, onScreen: true),
      CaptureWindowCandidate(title: nil, frame: CGRect(x: 1920, y: 0, width: 2000, height: 1200), layer: 0, onScreen: true),
    ]
    let accessible = [AccessibleWindowFrame(title: "Editor", frame: real)]
    let bound = boundAppWindowIndices(candidates: candidates, accessible: accessible, requireOnScreen: true, fallbackToOnScreen: true)
    XCTAssertEqual(bound, [0])
    XCTAssertEqual(nativeRecordingDisplayIndex(windowFrames: bound.map { candidates[$0].frame }, displayFrames: displays), 0)
    // The old rule, every layer-0 window, chose the backing window's display.
    XCTAssertEqual(nativeRecordingDisplayIndex(windowFrames: candidates.map(\.frame), displayFrames: displays), 1)
  }

  func testOffScreenAndNonWindowLayersAreLeftOut() {
    let candidates = [
      CaptureWindowCandidate(title: "Editor", frame: real, layer: 0, onScreen: false),
      CaptureWindowCandidate(title: "Editor", frame: real, layer: 101, onScreen: true),
      CaptureWindowCandidate(title: "Editor", frame: .zero, layer: 0, onScreen: true),
    ]
    let accessible = [AccessibleWindowFrame(title: "Editor", frame: real)]
    XCTAssertEqual(boundAppWindowIndices(candidates: candidates, accessible: accessible, requireOnScreen: true), [])
    // A screenshot may still capture the off-screen window on its own.
    XCTAssertEqual(boundAppWindowIndices(candidates: candidates, accessible: accessible, requireOnScreen: false), [0])
  }

  func testFrameMatchesWhenTitlesDiffer() {
    let candidates = [CaptureWindowCandidate(title: nil, frame: real.offsetBy(dx: 1, dy: -1), layer: 0, onScreen: true)]
    let accessible = [AccessibleWindowFrame(title: "Untitled", frame: real)]
    XCTAssertEqual(boundAppWindowIndices(candidates: candidates, accessible: accessible, requireOnScreen: true), [0])
  }

  func testWithoutAccessibilityWindowsOnlyRecordingFallsBackToOnScreenWindows() {
    let candidates = [
      CaptureWindowCandidate(title: nil, frame: real, layer: 0, onScreen: true),
      CaptureWindowCandidate(title: nil, frame: displays[1], layer: 0, onScreen: false),
    ]
    XCTAssertEqual(boundAppWindowIndices(candidates: candidates, accessible: [], requireOnScreen: true), [])
    XCTAssertEqual(boundAppWindowIndices(candidates: candidates, accessible: [], requireOnScreen: true, fallbackToOnScreen: true), [0])
  }

  func testScreenshotCropIsTheWindowInItsDisplaysOwnPoints() {
    XCTAssertEqual(appCaptureCrop(windowFrame: real, displayFrame: displays[0]), real)
    let onSecond = CGRect(x: 2000, y: 100, width: 800, height: 600)
    XCTAssertEqual(appCaptureCrop(windowFrame: onSecond, displayFrame: displays[1]), CGRect(x: 80, y: 100, width: 800, height: 600))
    // A window reaching past its display has no display crop; it is captured on its own.
    XCTAssertNil(appCaptureCrop(windowFrame: CGRect(x: 1800, y: 100, width: 400, height: 300), displayFrame: displays[0]))
    XCTAssertNil(appCaptureCrop(windowFrame: .zero, displayFrame: displays[0]))
  }

  func testRecordedAppVisibilityNeedsAnOnScreenNormalWindow() {
    let pid: pid_t = 4242
    func window(pid: pid_t, layer: Int, bounds: CGRect, alpha: Double = 1) -> [String: Any] {
      [
        kCGWindowOwnerPID as String: NSNumber(value: pid),
        kCGWindowLayer as String: NSNumber(value: layer),
        kCGWindowAlpha as String: NSNumber(value: alpha),
        kCGWindowBounds as String: bounds.dictionaryRepresentation as NSDictionary,
      ]
    }
    XCTAssertTrue(recordedAppHasVisibleWindow(windowInfo: [window(pid: pid, layer: 0, bounds: real)], pid: pid))
    // Hidden or minimized: only other apps' windows and the app's menu-level windows remain.
    XCTAssertFalse(recordedAppHasVisibleWindow(windowInfo: [
      window(pid: 1, layer: 0, bounds: real),
      window(pid: pid, layer: 25, bounds: real),
      window(pid: pid, layer: 0, bounds: .zero),
      window(pid: pid, layer: 0, bounds: real, alpha: 0),
    ], pid: pid))
  }

  func testAStreamEndedAfterFramesIsAStopReasonNotAFailure() {
    let details = ["frames": "120"]
    XCTAssertNil(nativeRecordingFinishFailure(interruption: "The user stopped the stream (SCStreamErrorDomain -3817)", sawFirstFrame: true, writerCompleted: true, details: details))
    XCTAssertNil(nativeRecordingFinishFailure(interruption: nil, sawFirstFrame: true, writerCompleted: true, details: details))
    guard case .commandFailed(let beforeFrame, _)? = nativeRecordingFinishFailure(interruption: "display removed", sawFirstFrame: false, writerCompleted: false, details: details) else {
      return XCTFail("a capture that ended before its first frame must fail")
    }
    XCTAssertTrue(beforeFrame.contains("before the first frame"))
    guard case .commandFailed(let unfinished, let unfinishedDetails)? = nativeRecordingFinishFailure(interruption: "display removed", sawFirstFrame: true, writerCompleted: false, details: details) else {
      return XCTFail("a writer that did not complete must fail")
    }
    XCTAssertEqual(unfinished, "recording did not finish")
    XCTAssertEqual(unfinishedDetails["interruption"], "display removed")
  }
}
