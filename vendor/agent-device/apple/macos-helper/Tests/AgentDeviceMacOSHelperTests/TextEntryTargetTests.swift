import AppKit
import Foundation
import XCTest
@testable import AgentDeviceMacOSHelper

/// Text entry binds to the session surface. These tests only read which app is frontmost; they
/// never activate an app or post an event.
final class TextEntryTargetTests: XCTestCase {
  private let stale = "com.teamofsilicons.extend.nonexistent.\(UUID().uuidString)"

  private func request(surface: String?) throws -> TextEntryRequest {
    var object: [String: Any] = ["text": "x", "replace": false, "bundleId": stale]
    if let surface { object["surface"] = surface }
    return try JSONDecoder().decode(TextEntryRequest.self, from: JSONSerialization.data(withJSONObject: object))
  }

  func testFrontmostSurfaceFollowsTheFrontmostAppNotTheBundleRecordedAtOpen() throws {
    do {
      let app = try resolveTextEntryApplication(try request(surface: "frontmost-app"))
      XCTAssertNotEqual(app.bundleIdentifier, stale)
      XCTAssertEqual(app.processIdentifier, NSWorkspace.shared.frontmostApplication?.processIdentifier)
    } catch HelperError.commandFailed(let message, let details) {
      // A test host without any frontmost app may refuse, but never because of the stale bundle.
      XCTAssertNil(details["bundleId"], message)
      XCTAssertEqual(message, "unable to resolve frontmost app")
    }
  }

  func testAppSurfaceAndLegacyRequestsKeepTheExplicitBundle() throws {
    for surface in ["app", nil] as [String?] {
      XCTAssertThrowsError(try resolveTextEntryApplication(try request(surface: surface))) { error in
        guard case HelperError.commandFailed(_, let details) = error else { return XCTFail("wrong error: \(error)") }
        XCTAssertEqual(details["bundleId"], stale)
      }
    }
  }

  func testSurfacesWithoutTextEntryAreRefused() throws {
    for surface in ["desktop", "menubar"] {
      XCTAssertThrowsError(try resolveTextEntryApplication(try request(surface: surface))) { error in
        guard case HelperError.invalidArgs = error else { return XCTFail("wrong error: \(error)") }
      }
    }
  }
}
