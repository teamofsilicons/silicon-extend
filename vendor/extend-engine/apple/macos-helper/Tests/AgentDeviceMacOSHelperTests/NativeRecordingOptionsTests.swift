import XCTest
@testable import AgentDeviceMacOSHelper

final class NativeRecordingOptionsTests: XCTestCase {
  func testDefaultsEnforceProductLimits() throws {
    let options = try NativeRecordingOptions(arguments: ["--out", "/tmp/test.mp4", "--status", "/tmp/status.json"])
    XCTAssertEqual(options.durationMs, 1_800_000)
    XCTAssertEqual(options.maxBytes, 1_073_741_824)
    XCTAssertEqual(options.fps, 30)
  }

  func testRejectsExpandingLimitsAndAmbiguousPaths() {
    let base = ["--out", "/tmp/test.mp4", "--status", "/tmp/status.json"]
    for invalid in [["--max-duration-ms", "1800001"], ["--max-bytes", "1073741825"], ["--fps", "0"], ["--fps", "61"], ["--fps", "abc"], ["--out", "/tmp/other.mp4"], ["--unknown", "x"]] {
      XCTAssertThrowsError(try NativeRecordingOptions(arguments: base + invalid))
    }
    XCTAssertThrowsError(try NativeRecordingOptions(arguments: ["--out", "relative.mp4", "--status", "/tmp/status.json"]))
    XCTAssertThrowsError(try NativeRecordingOptions(arguments: ["--out", "/tmp/same", "--status", "/tmp/same"]))
  }
}
