import CoreGraphics
import Foundation

public enum MouseClickButton: String, Sendable {
  case primary, secondary, middle
  public var cgButton: CGMouseButton { switch self { case .primary: return .left; case .secondary: return .right; case .middle: return .center } }
  public var downType: CGEventType { switch self { case .primary: return .leftMouseDown; case .secondary: return .rightMouseDown; case .middle: return .otherMouseDown } }
  public var upType: CGEventType { switch self { case .primary: return .leftMouseUp; case .secondary: return .rightMouseUp; case .middle: return .otherMouseUp } }
}

public struct MouseClickRequest: Equatable, Sendable {
  public let x: Double
  public let y: Double
  /// How long the button stays down. Zero asks for the default hold.
  public let holdMs: Int
  /// Independent presses, each at click state 1.
  public let clicks: Int
  /// Post every press as a double-click pair with a rising click state.
  public let doubleClick: Bool
  public let intervalMs: Int
  public let button: MouseClickButton

  public init(
    x: Double,
    y: Double,
    holdMs: Int = 0,
    clicks: Int = 1,
    doubleClick: Bool = false,
    intervalMs: Int = 120,
    button: MouseClickButton = .primary
  ) {
    self.x = x
    self.y = y
    self.holdMs = holdMs
    self.clicks = clicks
    self.doubleClick = doubleClick
    self.intervalMs = intervalMs
    self.button = button
  }
}

public enum MouseClickDeliveryError: Error, Equatable {
  case eventCreationFailed
}

/// The button the helper currently holds down, kept where a signal handler can reach it.
/// A helper killed between a mouse-down and its mouse-up would otherwise leave the system's
/// primary button stuck down for whatever the user touches next. The host stops the helper
/// with SIGTERM before SIGKILL (`runMacOsHelper` in `helper.ts`) so that this handler runs
/// on a deadline and on a cancelled request, not only on a signal sent by hand.
nonisolated(unsafe) private var heldMouseButton: (point: CGPoint, clickState: Int, button: MouseClickButton)?

private func releaseHeldMouseButton() {
  guard let held = heldMouseButton else { return }
  heldMouseButton = nil
  let up = CGEvent(
    mouseEventSource: nil,
    mouseType: held.button.upType,
    mouseCursorPosition: held.point,
    mouseButton: held.button.cgButton
  )
  up?.setIntegerValueField(.mouseEventClickState, value: Int64(held.clickState))
  up?.post(tap: .cghidEventTap)
}

private func installMouseReleaseOnTermination() {
  for terminationSignal in [SIGTERM, SIGINT, SIGHUP] {
    signal(terminationSignal) { received in
      releaseHeldMouseButton()
      _exit(128 + received)
    }
  }
}

/// Posts a click the way a trackpad would: one motion to the point, then each press held
/// long enough for the app to accept the release. A termination signal that lands inside a
/// hold releases the button before the process exits.
public func postMouseClick(_ request: MouseClickRequest) throws {
  installMouseReleaseOnTermination()
  let point = CGPoint(x: request.x, y: request.y)
  let hold = mouseClickHoldMs(requestedMs: request.holdMs)
  let presses = mouseClickPresses(
    clicks: request.clicks,
    doubleClick: request.doubleClick,
    intervalMs: request.intervalMs
  )

  guard let move = CGEvent(
    mouseEventSource: nil,
    mouseType: .mouseMoved,
    mouseCursorPosition: point,
    mouseButton: request.button.cgButton
  ) else {
    throw MouseClickDeliveryError.eventCreationFailed
  }
  move.post(tap: .cghidEventTap)

  for press in presses {
    if press.delayBeforeMs > 0 {
      usleep(UInt32(press.delayBeforeMs) * 1000)
    }
    guard let down = CGEvent(
      mouseEventSource: nil,
      mouseType: request.button.downType,
      mouseCursorPosition: point,
      mouseButton: request.button.cgButton
    ), let up = CGEvent(
      mouseEventSource: nil,
      mouseType: request.button.upType,
      mouseCursorPosition: point,
      mouseButton: request.button.cgButton
    ) else {
      throw MouseClickDeliveryError.eventCreationFailed
    }
    down.setIntegerValueField(.mouseEventClickState, value: Int64(press.clickState))
    up.setIntegerValueField(.mouseEventClickState, value: Int64(press.clickState))
    heldMouseButton = (point, press.clickState, request.button)
    down.post(tap: .cghidEventTap)
    usleep(UInt32(hold) * 1000)
    // The record clears only after the up is posted: a signal that lands between the two
    // would otherwise find nothing to release and exit with the button still down. A signal
    // that lands after the post releases a button already up, which is harmless.
    up.post(tap: .cghidEventTap)
    heldMouseButton = nil
  }
}
