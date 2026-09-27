// Loaded only inside the isolated Rust fixture. Reads only that process's own NSWindows.
// The production ui::run creates and manages every tested window and WKWebView.
import AppKit
import WebKit

private var timer: DispatchSourceTimer?
private var lastID = ""
private var focusSentinel: NSWindow?

private func webView(_ view: NSView?) -> WKWebView? {
    guard let view else { return nil }
    if let web = view as? WKWebView { return web }
    return view.subviews.lazy.compactMap { webView($0) }.first
}

@_cdecl("start_banner_probe")
public func startBannerProbe() {
    guard let path = ProcessInfo.processInfo.environment["EXTEND_BANNER_FIXTURE"] else { return }
    let root = URL(fileURLWithPath: path)
    let source = DispatchSource.makeTimerSource(queue: .main)
    source.schedule(deadline: .now() + 0.2, repeating: 0.025)
    source.setEventHandler {
        guard let data = try? Data(contentsOf: root.appendingPathComponent("probe-command.json")),
              let command = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = command["id"] as? String, id != lastID else { return }
        lastID = id
        func reply(_ result: [String: Any]) {
            var value = result
            value["id"] = id
            if let bytes = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) {
                try? bytes.write(to: root.appendingPathComponent("probe-response.json"), options: .atomic)
            }
        }
        if command["op"] as? String == "focus_sentinel" {
            if focusSentinel == nil {
                focusSentinel = NSWindow(contentRect: NSRect(x: 40, y: 40, width: 240, height: 80), styleMask: [.titled], backing: .buffered, defer: false)
                focusSentinel?.title = "Owned banner focus sentinel"
            }
            focusSentinel?.makeKeyAndOrderFront(nil)
            reply(["key_title":NSApplication.shared.keyWindow?.title ?? ""])
            return
        }
        if command["op"] as? String == "hide_sentinel" {
            focusSentinel?.orderOut(nil)
            reply(["hidden":true])
            return
        }
        guard let window = NSApplication.shared.windows.first(where: { $0.title == "Silicon Extend: in use" }),
              let web = webView(window.contentView) else { reply(["error":"banner not created"]); return }
        let op = command["op"] as? String ?? "snapshot"
        if op == "move", let x = command["x"] as? Double, let y = command["y"] as? Double {
            // A controlled AppKit move tests resize clamping, not physical dragging.
            let top = NSScreen.screens.first?.frame.maxY ?? 0
            window.setFrameTopLeftPoint(NSPoint(x: x, y: top - y))
        }
        let selector = command["selector"] as? String ?? ""
        let encoded = String(data: try! JSONSerialization.data(withJSONObject: [selector]), encoding: .utf8)!
        let click = op == "click" ? "document.querySelector((\(encoded))[0]).click();" : ""
        let script = """
        (() => { \(click)
          return {text: document.querySelector('#banner').innerText,
            collapsed: document.body.classList.contains('banner-minimized'),
            buttons: [...document.querySelectorAll('#banner button')].filter(e => e.getBoundingClientRect().width).map(e => {
              const r = e.getBoundingClientRect(); return {id:e.id,action:e.dataset.action,target:e.dataset.target||null,x:r.x,y:r.y,w:r.width,h:r.height};
            })}; })()
        """
        web.evaluateJavaScript(script) { value, error in
            let frame = window.frame
            let top = NSScreen.screens.first?.frame.maxY ?? 0
            let screen = window.screen?.frame ?? .zero
            reply(["pid":ProcessInfo.processInfo.processIdentifier,"window_number":window.windowNumber,"visible":window.isVisible,
                   "key":window.isKeyWindow,"level":window.level.rawValue,
                   "key_title":NSApplication.shared.keyWindow?.title ?? "",
                   "frame":["x":frame.minX,"y":top-frame.maxY,"width":frame.width,"height":frame.height],
                   "screen":["x":screen.minX,"y":top-screen.maxY,"width":screen.width,"height":screen.height],
                   "dom":value ?? NSNull(), "error":error?.localizedDescription ?? NSNull()])
        }
    }
    timer = source
    source.resume()
}
