// Local end-to-end fixture: exposes only its own text fields and their current values.
import AppKit
import Foundation

let output = URL(fileURLWithPath: CommandLine.arguments[1])
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(contentRect: NSRect(x: 180, y: 180, width: 560, height: 330),
                      styleMask: [.titled, .closable], backing: .buffered, defer: false)
window.title = "Bridge text input verification"
let first = NSTextField(string: "first value")
let second = NSTextField(string: "untouched")
let secure = NSSecureTextField(string: "")
let fields: [NSTextField] = [first, second, secure]
for (index, field) in fields.enumerated() {
  field.setAccessibilityIdentifier("bridge-field-\(index)")
  field.frame = NSRect(x: 24, y: 250 - index * 80, width: 490, height: 36)
  window.contentView!.addSubview(field)
}
window.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)
DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
  first.stringValue = "first value"
  window.makeFirstResponder(first)
}
let timer = Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { _ in
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
