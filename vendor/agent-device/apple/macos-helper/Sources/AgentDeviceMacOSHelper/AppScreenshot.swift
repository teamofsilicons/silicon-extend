import AppKit
import ApplicationServices
import CoreGraphics
import CoreImage
import CoreMedia
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

/// One ScreenCaptureKit window, reduced to the facts that decide whether it is one of the
/// app's real windows.
struct CaptureWindowCandidate {
  let title: String?
  let frame: CGRect
  let layer: Int
  let onScreen: Bool
}

/// One of the app's Accessibility windows: the windows the Carbon and the snapshot see.
struct AccessibleWindowFrame {
  let title: String?
  let frame: CGRect?
}

/// The app's real windows among its ScreenCaptureKit windows. ScreenCaptureKit also lists
/// untitled backing and auxiliary windows, some larger than the real one, so a window counts
/// only when it matches an Accessibility window by title or frame. With `requireOnScreen`,
/// windows that are minimized, hidden or on another Space are left out, because a display
/// capture shows nothing of them. `fallbackToOnScreen` keeps on-screen layer-0 windows when
/// the app exposes no Accessibility windows at all (for example when Accessibility access is
/// missing), so a recording can still choose a display.
func boundAppWindowIndices(
  candidates: [CaptureWindowCandidate],
  accessible: [AccessibleWindowFrame],
  requireOnScreen: Bool,
  fallbackToOnScreen: Bool = false
) -> [Int] {
  let usable = candidates.indices.filter { index in
    let candidate = candidates[index]
    return candidate.layer == 0 && !candidate.frame.isEmpty && (!requireOnScreen || candidate.onScreen)
  }
  if accessible.isEmpty {
    return fallbackToOnScreen ? usable.filter { candidates[$0].onScreen } : []
  }
  return usable.filter { index in
    let candidate = candidates[index]
    return accessible.contains { window in
      if let title = window.title, !title.isEmpty, title == candidate.title { return true }
      guard let rect = window.frame else { return false }
      return abs(rect.minX - candidate.frame.minX) < 2 && abs(rect.minY - candidate.frame.minY) < 2
        && abs(rect.width - candidate.frame.width) < 2 && abs(rect.height - candidate.frame.height) < 2
    }
  }
}

/// Where a bound window sits inside its display, in the display's own points, when the whole
/// window is on that display. Capturing that area of the display, filtered to the app, keeps the
/// app's menus, popovers and sheets over the window and leaves every other app out. A window that
/// reaches past its display has no such crop and is captured as a single window instead.
func appCaptureCrop(windowFrame: CGRect, displayFrame: CGRect) -> CGRect? {
  guard !windowFrame.isEmpty, !displayFrame.isEmpty,
        displayFrame.insetBy(dx: -1, dy: -1).contains(windowFrame) else { return nil }
  let local = windowFrame.offsetBy(dx: -displayFrame.minX, dy: -displayFrame.minY)
  let crop = local.intersection(CGRect(origin: .zero, size: displayFrame.size)).integral
  return crop.isEmpty ? nil : crop
}

func accessibleWindowFrames(pid: pid_t) -> [AccessibleWindowFrame] {
  windows(of: AXUIElementCreateApplication(pid)).map { element in
    AccessibleWindowFrame(
      title: stringAttribute(element, attribute: kAXTitleAttribute as String),
      frame: rectAttribute(element).map { CGRect(x: $0.x, y: $0.y, width: $0.width, height: $0.height) }
    )
  }
}

func captureWindowCandidates(_ windows: [SCWindow]) -> [CaptureWindowCandidate] {
  windows.map {
    CaptureWindowCandidate(title: $0.title, frame: $0.frame, layer: $0.windowLayer, onScreen: $0.isOnScreen)
  }
}

/// Backing scale of a display, for sizing a capture in pixels on every supported macOS.
func displayPointPixelScale(_ displayID: CGDirectDisplayID) -> CGFloat {
  let key = NSDeviceDescriptionKey("NSScreenNumber")
  let screen = NSScreen.screens.first { ($0.deviceDescription[key] as? NSNumber)?.uint32Value == displayID }
  return max(screen?.backingScaleFactor ?? 1, 1)
}

// Mutable state is guarded by `lock`; ScreenCaptureKit calls in on its own queue.
private final class SingleFrameOutput: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
  private let lock = NSLock()
  private var result: Result<CGImage, Error>?
  private var continuation: CheckedContinuation<CGImage, Error>?

  func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer, of type: SCStreamOutputType) {
    guard type == .screen, sample.isValid,
          let info = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
          let rawStatus = info.first?[.status] as? Int, SCFrameStatus(rawValue: rawStatus) == .complete,
          let pixels = CMSampleBufferGetImageBuffer(sample) else { return }
    let image = CIImage(cvPixelBuffer: pixels)
    guard let cgImage = CIContext().createCGImage(image, from: image.extent) else { return }
    finish(.success(cgImage))
  }

  func stream(_ stream: SCStream, didStopWithError error: Error) {
    finish(.failure(error))
  }

  func finish(_ value: Result<CGImage, Error>) {
    lock.lock()
    guard result == nil else { lock.unlock(); return }
    result = value
    let waiting = continuation
    continuation = nil
    lock.unlock()
    waiting?.resume(with: value)
  }

  func image(timeout: TimeInterval) async throws -> CGImage {
    try await withCheckedThrowingContinuation { continuation in
      lock.lock()
      if let result {
        lock.unlock()
        continuation.resume(with: result)
        return
      }
      self.continuation = continuation
      lock.unlock()
      DispatchQueue.global().asyncAfter(deadline: .now() + timeout) {
        self.finish(.failure(HelperError.commandFailed(
          "screenshot failed: ScreenCaptureKit delivered no frame within \(Int(timeout)) seconds. Make sure the display is awake and try again.",
          details: ["reason": "screenshot_no_frame"]
        )))
      }
    }
  }
}

/// One still image of a filter. macOS 14 and newer use the screenshot API; macOS 13 has only
/// streams, so it starts a stream, keeps its first complete frame and stops it.
func captureSingleFrame(filter: SCContentFilter, configuration: SCStreamConfiguration) async throws -> CGImage {
  if #available(macOS 14, *) {
    return try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration)
  }
  configuration.queueDepth = 3
  configuration.minimumFrameInterval = CMTime(value: 1, timescale: 30)
  configuration.pixelFormat = kCVPixelFormatType_32BGRA
  let output = SingleFrameOutput()
  let stream = SCStream(filter: filter, configuration: configuration, delegate: output)
  try stream.addStreamOutput(output, type: .screen, sampleHandlerQueue: DispatchQueue(label: "com.teamofsilicons.extend.screenshot"))
  try await stream.startCapture()
  do {
    let image = try await output.image(timeout: 5)
    try? await stream.stopCapture()
    return image
  } catch {
    try? await stream.stopCapture()
    throw error
  }
}

private func captureBoundAppImage(bundleId: String, fullscreen: Bool) async throws -> CGImage {
  let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
  guard let app = content.applications.first(where: { $0.bundleIdentifier == bundleId }) else {
    throw HelperError.commandFailed("the requested app is not available for capture", details: ["bundleId": bundleId])
  }
  let owned = content.windows.filter { $0.owningApplication?.processID == app.processID }
  let candidates = captureWindowCandidates(owned)
  let accessible = accessibleWindowFrames(pid: app.processID)
  let displayFrames = content.displays.map(\.frame)
  // Prefer the app's on-screen windows; a window that is only off screen (another Space, Stage
  // Manager) can still be captured on its own.
  var matched = boundAppWindowIndices(candidates: candidates, accessible: accessible, requireOnScreen: true)
  if matched.isEmpty {
    matched = boundAppWindowIndices(candidates: candidates, accessible: accessible, requireOnScreen: false)
  }
  guard let chosen = appCaptureWindowIndex(windowFrames: matched.map { owned[$0].frame }, displayFrames: displayFrames) else {
    throw HelperError.commandFailed(
      "the requested app has no window on a display to capture. Open or unminimize one of its windows, then take the screenshot again.",
      details: ["bundleId": bundleId, "reason": "app_window_not_found"]
    )
  }
  let window = owned[matched[chosen]]
  let displayIndex = nativeRecordingDisplayIndex(windowFrames: [window.frame], displayFrames: displayFrames)
  let display = displayIndex.map { content.displays[$0] }
  let config = SCStreamConfiguration()
  config.showsCursor = false
  let filter: SCContentFilter
  // The captured area in points: a crop of the display, or nil for the filter's whole content.
  var area: CGRect?
  if fullscreen {
    guard let display else {
      throw HelperError.commandFailed("the target app's window is on no display, so there is no display to capture")
    }
    // --fullscreen explicitly requests the whole display, including other applications.
    filter = SCContentFilter(display: display, excludingWindows: [])
    area = CGRect(origin: .zero, size: display.frame.size)
  } else if let display, window.isOnScreen,
            let crop = appCaptureCrop(windowFrame: window.frame, displayFrame: display.frame) {
    // The display, filtered to this app and cropped to its window: menus, popovers and sheets
    // the app shows over the window are in the image, other apps are not.
    filter = SCContentFilter(display: display, including: [app], exceptingWindows: [])
    config.sourceRect = crop
    area = crop
  } else {
    // Off screen or reaching past its display: the window alone, as ScreenCaptureKit renders it.
    filter = SCContentFilter(desktopIndependentWindow: window)
    if #available(macOS 14, *) { config.ignoreShadowsSingleWindow = true }
  }
  let size: CGSize
  let scale: CGFloat
  if #available(macOS 14, *) {
    size = area?.size ?? filter.contentRect.size
    scale = CGFloat(filter.pointPixelScale)
  } else {
    size = area?.size ?? window.frame.size
    scale = displayPointPixelScale(display?.displayID ?? CGMainDisplayID())
  }
  config.width = max(1, Int((size.width * scale).rounded()))
  config.height = max(1, Int((size.height * scale).rounded()))
  return try await captureSingleFrame(filter: filter, configuration: config)
}

func captureAppScreenshot(bundleId: String?, fullscreen: Bool, outPath: String) throws {
  // Validate the identity before acquiring any screen content. Never substitute frontmost.
  let app = try resolveTargetApplication(bundleId: bundleId, surface: "app")
  guard let identifier = app.bundleIdentifier else { throw HelperError.commandFailed("target app has no bundle identity") }
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

/// The main display as one image, for macOS versions without `captureImage(in:)`.
func captureMainDisplayImage() throws -> CGImage {
  _ = NSApplication.shared
  _ = NSScreen.screens
  var result: Result<CGImage, Error>?
  Task { @MainActor in
    do {
      let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
      guard let display = content.displays.first(where: { $0.displayID == CGMainDisplayID() }) else {
        throw HelperError.commandFailed("screenshot could not find the main display")
      }
      let config = SCStreamConfiguration()
      let scale = displayPointPixelScale(display.displayID)
      config.width = max(1, Int((display.frame.width * scale).rounded()))
      config.height = max(1, Int((display.frame.height * scale).rounded()))
      config.showsCursor = false
      result = .success(try await captureSingleFrame(
        filter: SCContentFilter(display: display, excludingWindows: []), configuration: config
      ))
    } catch { result = .failure(error) }
  }
  while result == nil { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
  return try result!.get()
}
