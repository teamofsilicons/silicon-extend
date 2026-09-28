import AppKit
import AVFoundation
import CoreGraphics
import Foundation
import ScreenCaptureKit

func nativeRecordingDisplayIndex(windowFrames: [CGRect], displayFrames: [CGRect]) -> Int? {
  var bestIndex: Int?
  var bestArea: CGFloat = 0
  for window in windowFrames where !window.isEmpty {
    for (index, display) in displayFrames.enumerated() {
      let intersection = window.intersection(display)
      guard !intersection.isNull, !intersection.isEmpty else { continue }
      let area = intersection.width * intersection.height
      if area > bestArea { bestIndex = index; bestArea = area }
    }
  }
  return bestIndex
}

/// Whether the recorded app can still appear in an app-scoped recording.
enum RecordedAppState: Equatable {
  case visible
  case notVisible
  case exited
}

/// The app shows in a recording only through a layer-0 window that is on screen. A hidden app
/// (Command-H), a minimized window or a window on another Space leaves the frames without it.
func recordedAppHasVisibleWindow(windowInfo: [[String: Any]], pid: pid_t) -> Bool {
  windowInfo.contains { window in
    guard (window[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid,
          (window[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
          ((window[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 1) > 0,
          let rawBounds = window[kCGWindowBounds as String] as? NSDictionary,
          let bounds = CGRect(dictionaryRepresentation: rawBounds as CFDictionary),
          !bounds.isEmpty
    else { return false }
    return true
  }
}

func observeRecordedApp(pid: pid_t) -> RecordedAppState {
  if kill(pid, 0) != 0 && errno == ESRCH { return .exited }
  guard let info = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else {
    // No window list is no evidence either way; only a list without the app counts.
    return .visible
  }
  return recordedAppHasVisibleWindow(windowInfo: info, pid: pid) ? .visible : .notVisible
}

/// Whether a stopped recording failed. A stream macOS ended after frames were written is a stop
/// reason ("interrupted"): the writer still closes the MP4, so the video up to that point is
/// complete and the helper exits 0. Only a writer that did not complete, or a capture that
/// ended before its first frame, is a failure.
func nativeRecordingFinishFailure(
  interruption: String?,
  sawFirstFrame: Bool,
  writerCompleted: Bool,
  details: [String: String]
) -> HelperError? {
  if let interruption, !sawFirstFrame {
    return .commandFailed("macOS stopped the screen capture before the first frame: \(interruption)", details: details)
  }
  if !writerCompleted {
    var details = details
    if let interruption { details["interruption"] = interruption }
    return .commandFailed("recording did not finish", details: details)
  }
  return nil
}

/// ScreenCaptureKit refuses a capture Screen Recording is not allowed for with SCStreamError
/// userDeclined (-3801). That refusal names the permission and a reason the caller turns into a
/// way forward; any other error is passed on unchanged.
func nativeRecordingStartError(_ error: Error) -> Error {
  let nsError = error as NSError
  guard nsError.domain == "com.apple.ScreenCaptureKit.SCStreamErrorDomain", nsError.code == -3801 else { return error }
  return HelperError.commandFailed(
    "macOS refused the screen capture because Screen Recording is not allowed for Silicon Extend",
    details: ["reason": "screen_recording_permission_denied", "permission": "screen-recording"]
  )
}

struct NativeRecordingOptions {
  let path: String
  let statusPath: String
  let bundleId: String?
  let fps: Int
  let durationMs: Int
  let maxBytes: Int

  init(arguments: [String]) throws {
    var values: [String: String] = [:]
    guard arguments.count.isMultiple(of: 2) else { throw HelperError.invalidArgs("record expects named option/value pairs") }
    let names = ["--out", "--status", "--bundle-id", "--fps", "--max-duration-ms", "--max-bytes"]
    for index in stride(from: 0, to: arguments.count, by: 2) {
      let key = arguments[index]
      guard names.contains(key), values[key] == nil else { throw HelperError.invalidArgs("unknown or repeated recording option") }
      values[key] = arguments[index + 1]
    }
    guard let path = values["--out"], path.hasPrefix("/"),
          let statusPath = values["--status"], statusPath.hasPrefix("/"), statusPath != path,
          let fps = Int(values["--fps"] ?? "30"), (1...60).contains(fps),
          let duration = Int(values["--max-duration-ms"] ?? "1800000"), (100...1_800_000).contains(duration),
          let maxBytes = Int(values["--max-bytes"] ?? "1073741824"), (1_048_576...1_073_741_824).contains(maxBytes)
    else { throw HelperError.invalidArgs("record needs absolute output/status paths, fps 1–60, duration at most 30 minutes and size at most 1 GiB") }
    self.path = path
    self.statusPath = statusPath
    self.bundleId = values["--bundle-id"]
    self.fps = fps
    self.durationMs = duration
    self.maxBytes = maxBytes
  }
}

private final class NativeScreenRecorder: NSObject, SCStreamOutput, SCStreamDelegate {
  private let options: NativeRecordingOptions
  private let queue = DispatchQueue(label: "com.teamofsilicons.extend.recording")
  private var writer: AVAssetWriter!
  private var input: AVAssetWriterInput!
  private var firstTime: CMTime?
  private var lastFrame: CMSampleBuffer?
  private var firstWallTime: TimeInterval?
  private var failure: Error?
  /// macOS ended the stream: the Carbon stopped it from the screen-recording indicator, the
  /// recorded display went away, or the system revoked capture. The frames written until then
  /// are still a complete video, so this is a stop reason, not a failure.
  private var interruption: Error?
  private var appPid: pid_t?
  private var appNotVisibleSeconds: Double = 0
  private var frames = 0
  private var signals: [DispatchSourceSignal] = []
  private var stopReason: String?
  private let parent = getppid()
  private var width = 0
  private var height = 0

  init(options: NativeRecordingOptions) { self.options = options }

  func stream(_ stream: SCStream, didStopWithError error: Error) {
    queue.async { self.interruption = self.interruption ?? error }
  }

  func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer, of type: SCStreamOutputType) {
    guard type == .screen, sample.isValid, failure == nil, interruption == nil,
          let info = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
          let rawStatus = info.first?[.status] as? Int, SCFrameStatus(rawValue: rawStatus) == .complete,
          CMSampleBufferGetImageBuffer(sample) != nil else { return }
    guard input.isReadyForMoreMediaData else { return }
    if firstTime == nil {
      firstTime = sample.presentationTimeStamp
      firstWallTime = ProcessInfo.processInfo.systemUptime
      writer.startSession(atSourceTime: sample.presentationTimeStamp)
    }
    guard input.append(sample) else {
      failure = HelperError.commandFailed("could not append a recording frame", details: [
        "frames": String(frames), "pts": String(sample.presentationTimeStamp.seconds),
        "previousPts": String(lastFrame?.presentationTimeStamp.seconds ?? -1),
        "writerError": String(describing: writer.error)
      ])
      return
    }
    frames += 1
    lastFrame = sample
    if frames == 1 {
      do { try writeStatus("recording") } catch { failure = error }
    }
  }

  private func writeStatus(_ state: String) throws {
    var status: [String: Any] = ["state": state, "path": options.path, "frames": frames,
                               "width": width, "height": height, "fps": options.fps,
                               "backend": "ScreenCaptureKit", "pid": getpid()]
    if let stopReason { status["reason"] = stopReason }
    if let failure { status["error"] = String(describing: failure) }
    if let interruption { status["interruption"] = describeInterruption(interruption) }
    if appNotVisibleSeconds > 0 { status["appNotVisibleMs"] = Int(appNotVisibleSeconds * 1000) }
    try JSONSerialization.data(withJSONObject: status).write(to: URL(fileURLWithPath: options.statusPath), options: .atomic)
  }

  private func describeInterruption(_ error: Error) -> String {
    let nsError = error as NSError
    return "\(nsError.localizedDescription) (\(nsError.domain) \(nsError.code))"
  }

  func run() async throws -> [String: String] {
    // Preflight can describe the launching terminal rather than this signed helper. The actual
    // ScreenCaptureKit acquisition remains authoritative and returns the OS permission error.
    let content: SCShareableContent
    do {
      content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: false)
    } catch {
      throw nativeRecordingStartError(error)
    }
    let display: SCDisplay
    let filter: SCContentFilter
    if let bundle = options.bundleId {
      guard let app = content.applications.first(where: { $0.bundleIdentifier == bundle }) else {
        throw HelperError.commandFailed(
          "the requested app is not running or has no window macOS can capture",
          details: ["reason": "app_not_available", "bundleId": bundle]
        )
      }
      // Choose the display from the app's real, on-screen windows, as screenshots do: larger
      // invisible backing windows must not move the recording to a display without the app.
      let owned = content.windows.filter { $0.owningApplication?.processID == app.processID }
      let bound = boundAppWindowIndices(
        candidates: captureWindowCandidates(owned),
        accessible: accessibleWindowFrames(pid: app.processID),
        requireOnScreen: true,
        fallbackToOnScreen: true
      )
      guard let displayIndex = nativeRecordingDisplayIndex(windowFrames: bound.map { owned[$0].frame }, displayFrames: content.displays.map(\.frame)) else {
        throw HelperError.commandFailed(
          "the requested app has no window on screen to record. Show one of its windows on the current Space (unhide the app or unminimize the window), then start the recording again.",
          details: [
            "reason": "app_window_not_on_screen",
            "bundleId": bundle, "pid": String(app.processID),
            "windows": owned.map { "\($0.frame) layer=\($0.windowLayer) onScreen=\($0.isOnScreen)" }.joined(separator: ";"),
            "displays": content.displays.map { String(describing: $0.frame) }.joined(separator: ";")
          ]
        )
      }
      display = content.displays[displayIndex]
      appPid = app.processID
      filter = SCContentFilter(display: display, including: [app], exceptingWindows: [])
    } else {
      guard let main = content.displays.first(where: { $0.displayID == CGMainDisplayID() }) else {
        throw HelperError.commandFailed("the main display is unavailable")
      }
      display = main
      filter = SCContentFilter(display: display, excludingWindows: [])
    }
    width = max(2, display.width / 2 * 2)
    height = max(2, display.height / 2 * 2)
    let configuration = SCStreamConfiguration()
    configuration.width = width
    configuration.height = height
    configuration.minimumFrameInterval = CMTime(value: 1, timescale: Int32(options.fps))
    configuration.pixelFormat = kCVPixelFormatType_32BGRA
    configuration.queueDepth = 3
    configuration.showsCursor = true
    configuration.capturesAudio = false
    let url = URL(fileURLWithPath: options.path)
    guard !FileManager.default.fileExists(atPath: options.path) else { throw HelperError.commandFailed("recording output already exists") }
    try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
    input = AVAssetWriterInput(mediaType: .video, outputSettings: [
      AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: width, AVVideoHeightKey: height,
      AVVideoCompressionPropertiesKey: [AVVideoAverageBitRateKey: 8_000_000, AVVideoMaxKeyFrameIntervalKey: options.fps * 2]
    ])
    input.expectsMediaDataInRealTime = true
    guard writer.canAdd(input) else { throw HelperError.commandFailed("cannot encode this display") }
    writer.add(input)
    guard writer.startWriting() else { throw writer.error ?? HelperError.commandFailed("could not start the MP4 writer") }
    for number in [SIGINT, SIGTERM, SIGHUP] {
      signal(number, SIG_IGN)
      let source = DispatchSource.makeSignalSource(signal: number, queue: queue)
      source.setEventHandler { self.stopReason = self.stopReason ?? "stopped" }
      source.resume()
      signals.append(source)
    }
    let stream = SCStream(filter: filter, configuration: configuration, delegate: self)
    try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
    do { try await stream.startCapture() } catch { writer.cancelWriting(); throw nativeRecordingStartError(error) }
    let started = ProcessInfo.processInfo.systemUptime
    var lastAppCheck = started
    while true {
      let now = ProcessInfo.processInfo.systemUptime
      let elapsed = now - started
      let size = (try? FileManager.default.attributesOfItem(atPath: options.path)[.size] as? NSNumber)?.intValue ?? 0
      // Once a second, and at once when the stream ends: an app that quit usually ends the
      // stream too, and "the app quit" is the more useful reason to report.
      var appState: RecordedAppState?
      var appInterval: Double = 0
      let interrupted = queue.sync { interruption != nil }
      if let appPid, interrupted || now - lastAppCheck >= 1 {
        appState = observeRecordedApp(pid: appPid)
        appInterval = now - lastAppCheck
        lastAppCheck = now
      }
      let shouldStop = queue.sync { () -> Bool in
        if getppid() != parent { stopReason = "owner-exited" }
        switch appState {
        case .exited?: stopReason = stopReason ?? "app-exited"
        case .notVisible?: appNotVisibleSeconds += appInterval
        default: break
        }
        if interruption != nil { stopReason = stopReason ?? "interrupted" }
        if elapsed * 1000 >= Double(options.durationMs) { stopReason = "duration-limit" }
        // Reserve space for queued frames and the closing MP4 metadata. The final file is checked too.
        if size >= options.maxBytes - min(16_777_216, options.maxBytes / 4) { stopReason = "size-limit" }
        if firstTime == nil && elapsed > 10 {
          failure = HelperError.commandFailed(
            "macOS delivered no screen frames within 10 seconds of starting the capture",
            details: ["reason": "no_screen_frames"]
          )
        }
        return stopReason != nil || failure != nil
      }
      if shouldStop { break }
      try await Task.sleep(nanoseconds: 50_000_000)
    }
    // A stream macOS already ended cannot be stopped again; that error says nothing new.
    do { try await stream.stopCapture() } catch { queue.sync { if interruption == nil { failure = failure ?? error } } }
    queue.sync {
      if let firstTime, let firstWallTime, let lastFrame, input.isReadyForMoreMediaData {
        let elapsed = min(ProcessInfo.processInfo.systemUptime - firstWallTime, Double(options.durationMs) / 1000)
        let end = CMTimeAdd(firstTime, CMTime(seconds: elapsed, preferredTimescale: 600))
        let frameDuration = CMTime(value: 1, timescale: Int32(options.fps))
        let tailTime = CMTimeSubtract(end, frameDuration)
        if CMTimeCompare(tailTime, lastFrame.presentationTimeStamp) > 0 {
          var timing = CMSampleTimingInfo(duration: frameDuration, presentationTimeStamp: tailTime, decodeTimeStamp: .invalid)
          var tail: CMSampleBuffer?
          if CMSampleBufferCreateCopyWithNewTiming(allocator: kCFAllocatorDefault, sampleBuffer: lastFrame, sampleTimingEntryCount: 1, sampleTimingArray: &timing, sampleBufferOut: &tail) == noErr,
             let tail { _ = input.append(tail) }
        }
        writer.endSession(atSourceTime: end)
      }
      input.markAsFinished()
    }
    if queue.sync(execute: { firstTime != nil }) { await writer.finishWriting() } else { writer.cancelWriting() }
    signals.forEach { $0.cancel() }
    signals.removeAll()
    let finalSize = (try? FileManager.default.attributesOfItem(atPath: options.path)[.size] as? NSNumber)?.intValue ?? 0
    try queue.sync {
      failure = failure ?? nativeRecordingFinishFailure(
        interruption: interruption.map(describeInterruption),
        sawFirstFrame: firstTime != nil,
        writerCompleted: writer.status == .completed,
        details: [
          "frames": String(frames), "firstPts": String(firstTime?.seconds ?? -1),
          "lastPts": String(lastFrame?.presentationTimeStamp.seconds ?? -1),
          "writerError": String(describing: writer.error)
        ]
      )
      if finalSize > options.maxBytes {
        failure = HelperError.commandFailed("recording exceeded its file size limit")
        try FileManager.default.removeItem(atPath: options.path)
      }
      try writeStatus(failure == nil ? "completed" : "failed")
      if let failure { throw failure }
    }
    var result = ["path": options.path, "reason": stopReason ?? "stopped", "backend": "ScreenCaptureKit"]
    queue.sync {
      if let interruption { result["interruption"] = describeInterruption(interruption) }
      if appNotVisibleSeconds > 0 { result["appNotVisibleMs"] = String(Int(appNotVisibleSeconds * 1000)) }
    }
    return result
  }
}

extension AgentDeviceMacOSHelper {
  static func handleScreenRecording(arguments: [String]) throws -> any Encodable {
    let recorder = NativeScreenRecorder(options: try NativeRecordingOptions(arguments: arguments))
    var result: Result<[String: String], Error>?
    Task { @MainActor in
      do { result = .success(try await recorder.run()) } catch { result = .failure(error) }
    }
    while result == nil { RunLoop.current.run(until: Date().addingTimeInterval(0.02)) }
    return SuccessEnvelope(data: try result!.get())
  }
}
