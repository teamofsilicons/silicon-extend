import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

struct TextEntryRequest: Decodable {
  let text: String
  let replace: Bool
  let bundleId: String?
  let x: Double?
  let y: Double?
  let delayMs: Int?
  let focusOnly: Bool?
}

struct TextEntryResponse: Encodable {
  let backend = "macos-helper"
  let bundleId: String?
  let characters: Int
}

nonisolated(unsafe) private var heldTextKey: (pid: pid_t, code: CGKeyCode)?

private func releaseTextKey() {
  guard let held = heldTextKey else { return }
  let event = CGEvent(keyboardEventSource: nil, virtualKey: held.code, keyDown: false)
  event?.flags = []
  event?.postToPid(held.pid)
  heldTextKey = nil
}

private func postTextKey(pid: pid_t, code: CGKeyCode, flags: CGEventFlags = [], text: String? = nil) throws {
  guard let down = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: true),
        let up = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: false)
  else { throw HelperError.commandFailed("could not create keyboard events") }
  down.flags = flags
  up.flags = []
  if let text {
    let units = Array(text.utf16)
    down.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
    up.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
  }
  heldTextKey = (pid, code)
  down.postToPid(pid)
  up.postToPid(pid)
  heldTextKey = nil
}

private func textElement(_ initial: AXUIElement) -> AXUIElement? {
  var candidate: AXUIElement? = initial
  for _ in 0..<12 {
    guard let element = candidate else { return nil }
    if ["AXTextField", "AXTextArea", "AXComboBox"].contains(stringAttribute(element, attribute: kAXRoleAttribute as String) ?? "") {
      return element
    }
    candidate = elementAttribute(element, attribute: kAXParentAttribute as String)
  }
  return nil
}

private func focusedTextElement() -> AXUIElement? {
  elementAttribute(AXUIElementCreateSystemWide(), attribute: kAXFocusedUIElementAttribute as String).flatMap(textElement)
}

private func exactTextValue(_ element: AXUIElement) -> String? {
  var value: CFTypeRef?
  guard AXUIElementCopyAttributeValue(element, kAXValueAttribute as CFString, &value) == .success else { return nil }
  return value as? String
}

private func selectText(_ element: AXUIElement, replace: Bool) throws -> Int {
  var countValue: CFTypeRef?
  let status = AXUIElementCopyAttributeValue(element, kAXNumberOfCharactersAttribute as CFString, &countValue)
  let count = status == .success ? (countValue as? NSNumber)?.intValue : exactTextValue(element)?.utf16.count
  guard let count, count >= 0 else { throw HelperError.commandFailed("could not read the text field's character count") }
  var range = CFRange(location: replace ? 0 : count, length: replace ? count : 0)
  guard let value = AXValueCreate(.cfRange, &range),
        AXUIElementSetAttributeValue(element, kAXSelectedTextRangeAttribute as CFString, value) == .success
  else { throw HelperError.commandFailed("could not select text in the focused field") }
  return count
}

private func requireTextFocus(_ element: AXUIElement, app: NSRunningApplication) throws {
  guard NSWorkspace.shared.frontmostApplication?.processIdentifier == app.processIdentifier,
        let focused = focusedTextElement(), CFEqual(focused, element)
  else {
    throw HelperError.commandFailed("the intended text field does not have keyboard focus", details: [
      "reason": "text_entry_focus_not_observed",
      "expectedPid": String(app.processIdentifier),
      "frontmostPid": String(NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0),
      "focusedRole": focusedTextElement().flatMap { stringAttribute($0, attribute: kAXRoleAttribute as String) } ?? "none"
    ])
  }
}

extension AgentDeviceMacOSHelper {
  static func handleTextEntry() throws -> any Encodable {
    let data = FileHandle.standardInput.readDataToEndOfFile()
    guard data.count <= 1_048_576 else { throw HelperError.invalidArgs("text input exceeds 1 MiB") }
    let request = try JSONDecoder().decode(TextEntryRequest.self, from: data)
    guard (0...1000).contains(request.delayMs ?? 0), request.x?.isFinite != false,
          request.y?.isFinite != false, (request.x == nil) == (request.y == nil)
    else { throw HelperError.invalidArgs("invalid text coordinates or delay") }
    guard AXIsProcessTrusted() else {
      throw HelperError.commandFailed("allow Accessibility for Silicon Bridge", details: ["permission": "accessibility"])
    }
    let app = try resolveTargetApplication(bundleId: request.bundleId, surface: nil)
    guard app.activate(options: [.activateIgnoringOtherApps]) else {
      throw HelperError.commandFailed("could not activate the target app")
    }
    let activationDeadline = Date().addingTimeInterval(1)
    while NSWorkspace.shared.frontmostApplication?.processIdentifier != app.processIdentifier && Date() < activationDeadline {
      RunLoop.current.run(until: Date().addingTimeInterval(0.01))
    }
    let element: AXUIElement
    if let x = request.x, let y = request.y {
      var hit: AXUIElement?
      guard AXUIElementCopyElementAtPosition(AXUIElementCreateSystemWide(), Float(x), Float(y), &hit) == .success,
            let hit, let target = textElement(hit)
      else { throw HelperError.commandFailed("no text field at the supplied coordinates") }
      var pid: pid_t = 0
      guard AXUIElementGetPid(target, &pid) == .success, pid == app.processIdentifier else {
        throw HelperError.commandFailed("the text field belongs to a different app")
      }
      guard AXUIElementSetAttributeValue(target, kAXFocusedAttribute as CFString, kCFBooleanTrue) == .success else {
        throw HelperError.commandFailed("could not focus the text field")
      }
      element = target
    } else {
      guard let focused = focusedTextElement() else { throw HelperError.commandFailed("no focused text field") }
      element = focused
    }
    let focusDeadline = Date().addingTimeInterval(1)
    while Date() < focusDeadline {
      if let focused = focusedTextElement(), CFEqual(focused, element) { break }
      RunLoop.current.run(until: Date().addingTimeInterval(0.01))
    }
    try requireTextFocus(element, app: app)
    if request.focusOnly == true {
      return SuccessEnvelope(data: TextEntryResponse(bundleId: app.bundleIdentifier, characters: 0))
    }
    for terminationSignal in [SIGTERM, SIGINT, SIGHUP] {
      signal(terminationSignal) { received in releaseTextKey(); _exit(128 + received) }
    }
    let pid = app.processIdentifier
    if request.replace {
      let count = try selectText(element, replace: true)
      if count > 0 { try postTextKey(pid: pid, code: 51) }
    } else if request.text != "\n" {
      _ = try selectText(element, replace: false)
    }
    var pending = ""
    func flushText() throws {
      guard !pending.isEmpty else { return }
      try requireTextFocus(element, app: app)
      try postTextKey(pid: pid, code: 0, text: pending)
      pending = ""
      Thread.sleep(forTimeInterval: Double(max(request.delayMs ?? 0, 2)) / 1000)
    }
    for character in request.text {
      try requireTextFocus(element, app: app)
      switch character {
      case "\n", "\r\n":
        try flushText()
        try postTextKey(pid: pid, code: 36)
      case "\t":
        try flushText()
        try postTextKey(pid: pid, code: 48)
      default:
        if pending.utf16.count + String(character).utf16.count > 20 { try flushText() }
        pending.append(character)
        if (request.delayMs ?? 0) > 0 { try flushText() }
      }
    }
    try flushText()
    if request.replace, stringAttribute(element, attribute: kAXSubroleAttribute as String) != "AXSecureTextField" {
      let deadline = Date().addingTimeInterval(1)
      while exactTextValue(element) != request.text && Date() < deadline {
        Thread.sleep(forTimeInterval: 0.01)
      }
      guard exactTextValue(element) == request.text else {
        throw HelperError.commandFailed("the text field did not contain the requested value", details: ["reason": "text_entry_verification_failed"])
      }
    }
    return SuccessEnvelope(data: TextEntryResponse(bundleId: app.bundleIdentifier, characters: request.text.count))
  }
}
