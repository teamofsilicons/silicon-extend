import AppKit
import ApplicationServices
import CoreGraphics
import Foundation
import ScreenCaptureKit

// Ignore empty/offscreen auxiliary windows; no foreground-app lookup participates in selection.
func appCaptureWindowIndex(windowFrames: [CGRect], displayFrames: [CGRect]) -> Int? {
  var best: Int?
  var bestArea: CGFloat = 0
  for (index, window) in windowFrames.enumerated() where !window.isEmpty {
    for display in displayFrames {
      let intersection = window.intersection(display)
      guard !intersection.isNull, !intersection.isEmpty else { continue }
      let area = intersection.width * intersection.height
      if area > bestArea { best = index; bestArea = area }
    }
  }
  return best
}

@available(macOS 14, *)
private func captureBoundAppImage(bundleId: String, fullscreen: Bool) async throws -> CGImage {
  let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
  guard let app = content.applications.first(where: { $0.bundleIdentifier == bundleId }) else {
    throw HelperError.commandFailed("the requested app is not available for capture", details: ["bundleId": bundleId])
  }
  // ScreenCaptureKit also lists untitled backing/auxiliary windows. Bind screenshots to
  // the app's accessibility windows instead of choosing a larger invisible backing surface.
  let accessibleWindows = windows(of: AXUIElementCreateApplication(app.processID))
  let windows = content.windows.filter { candidate in
    guard candidate.owningApplication?.processID == app.processID, candidate.windowLayer == 0 else { return false }
    return accessibleWindows.contains { element in
      if let title = stringAttribute(element, attribute: kAXTitleAttribute as String), !title.isEmpty,
         title == candidate.title { return true }
      guard let rect = rectAttribute(element) else { return false }
      return abs(rect.x - candidate.frame.minX) < 2 && abs(rect.y - candidate.frame.minY) < 2
        && abs(rect.width - candidate.frame.width) < 2 && abs(rect.height - candidate.frame.height) < 2
    }
  }
  guard let index = appCaptureWindowIndex(windowFrames: windows.map(\.frame), displayFrames: content.displays.map(\.frame)) else {
    throw HelperError.commandFailed("the requested app has no capturable window", details: ["bundleId": bundleId])
  }
  let window = windows[index]
  let filter: SCContentFilter
  if fullscreen {
    guard let displayIndex = nativeRecordingDisplayIndex(windowFrames: [window.frame], displayFrames: content.displays.map(\.frame)) else {
      throw HelperError.commandFailed("the target app display is unavailable")
    }
    // --fullscreen explicitly requests the whole display, including other applications.
    filter = SCContentFilter(display: content.displays[displayIndex], excludingWindows: [])
  } else {
    filter = SCContentFilter(desktopIndependentWindow: window)
  }
  let config = SCStreamConfiguration()
  config.width = max(1, Int(filter.contentRect.width * CGFloat(filter.pointPixelScale)))
  config.height = max(1, Int(filter.contentRect.height * CGFloat(filter.pointPixelScale)))
  config.showsCursor = false
  config.ignoreShadowsSingleWindow = true
  return try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
}

func captureAppScreenshot(bundleId: String?, fullscreen: Bool, outPath: String) throws {
  // Validate the identity before acquiring any screen content. Never substitute frontmost.
  let app = try resolveTargetApplication(bundleId: bundleId, surface: "app")
  guard let identifier = app.bundleIdentifier else { throw HelperError.commandFailed("target app has no bundle identity") }
  guard #available(macOS 14, *) else { throw HelperError.commandFailed("app screenshots require macOS 14 or newer") }
  // A command-line helper has no NSApplication startup. Window filters query WindowServer
  // display geometry; initialize AppKit on the main thread before that query.
  _ = NSApplication.shared
  _ = NSScreen.screens
  var result: Result<CGImage, Error>?
  Task { @MainActor in
    do { result = .success(try await captureBoundAppImage(bundleId: identifier, fullscreen: fullscreen)) }
    catch { result = .failure(error) }
  }
  while result == nil { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
  try writeScreenshotPNG(result!.get(), outPath: outPath)
}
