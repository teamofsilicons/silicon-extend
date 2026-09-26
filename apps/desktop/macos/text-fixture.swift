// Local end-to-end fixture: exposes only its own text fields and their current values.
import AppKit
import Foundation

final class RecordingPattern: NSView {
  var tick = 0
  override func draw(_ dirtyRect: NSRect) {
    for y in stride(from: 0, to: Int(bounds.height), by: 12) {
      for x in stride(from: 0, to: Int(bounds.width), by: 12) {
        let hue = CGFloat((x * 17 + y * 31 + tick * (x % 7 + y % 11 + 3)) % 360) / 360
        NSColor(calibratedHue: hue, saturation: 0.75, brightness: 0.55, alpha: 1).setFill()
        NSRect(x: x, y: y, width: 12, height: 12).fill()
      }
    }
  }
}

let output = URL(fileURLWithPath: CommandLine.arguments[1])
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(contentRect: NSRect(x: 180, y: 180, width: 560, height: 330),
                      styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "Extend text input verification"
if #available(macOS 13, *) { window.collectionBehavior = [.canJoinAllApplications] }
if let screen = NSScreen.main {
  window.setFrameTopLeftPoint(NSPoint(x: screen.visibleFrame.minX + 40, y: screen.visibleFrame.maxY - 80))
}
let pattern: RecordingPattern? = CommandLine.arguments.contains("--noise") ? RecordingPattern(frame: window.contentView!.bounds) : nil
if let pattern { window.contentView!.addSubview(pattern) }
let peer = CommandLine.arguments.contains("--peer")
let first = NSTextField(string: "first value")
let second = NSTextField(string: "untouched")
let secure = NSSecureTextField(string: "")
let fields: [NSTextField] = [first, second, secure]
for (index, field) in fields.enumerated() {
  field.setAccessibilityIdentifier("\(peer ? "peer" : "extend")-field-\(index)")
  field.frame = NSRect(x: 24, y: 250 - index * 80, width: 490, height: 36)
  window.contentView!.addSubview(field)
}
if peer {
  window.title = "Extend focus-change verification (other app)"
  window.backgroundColor = .systemPink
}
window.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)
DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
  first.stringValue = "first value"
  window.makeFirstResponder(first)
}
var frame = 0
let animated = CommandLine.arguments.contains("--animate")
let timer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { _ in
  if animated {
    frame += 1
    first.stringValue = "Recording frame \(frame)"
    pattern?.tick = frame
    pattern?.needsDisplay = true
  }
  let values = fields.map { field -> [String: Any] in
    let rect = window.convertToScreen(field.convert(field.bounds, to: nil))
    return ["value": field.stringValue, "x": rect.midX,
            "y": NSScreen.screens[0].frame.maxY - rect.midY]
  }
  if let data = try? JSONSerialization.data(withJSONObject: values) {
    try? data.write(to: output, options: .atomic)
  }
}
app.run()
