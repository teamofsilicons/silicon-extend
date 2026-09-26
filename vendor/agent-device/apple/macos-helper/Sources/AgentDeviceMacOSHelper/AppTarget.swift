import AppKit
import ApplicationServices
import Foundation

func resolveAppBundleAtPath(_ path: String) throws -> [String: String] {
  let expanded = (path as NSString).expandingTildeInPath
  let url = URL(fileURLWithPath: expanded).standardizedFileURL
  guard url.pathExtension.lowercased() == "app", let bundle = Bundle(url: url),
        let identifier = bundle.bundleIdentifier else {
    throw HelperError.commandFailed("the path is not an application bundle", details: ["path": expanded])
  }
  return ["bundleId": try validatedBundleId(identifier), "path": url.path]
}

func activateTargetApplication(_ app: NSRunningApplication) throws {
  guard app.activate(options: [.activateIgnoringOtherApps]) else {
    throw HelperError.commandFailed("could not activate the target app")
  }
  let deadline = Date().addingTimeInterval(1)
  while NSWorkspace.shared.frontmostApplication?.processIdentifier != app.processIdentifier && Date() < deadline {
    RunLoop.current.run(until: Date().addingTimeInterval(0.01))
  }
  guard NSWorkspace.shared.frontmostApplication?.processIdentifier == app.processIdentifier else {
    throw HelperError.commandFailed("target app did not become active", details: ["reason": "app_activation_not_observed"])
  }
}

func requirePointOwnedByApplication(x: Double, y: Double, app: NSRunningApplication) throws {
  var hit: AXUIElement?
  var pid: pid_t = 0
  guard AXUIElementCopyElementAtPosition(AXUIElementCreateSystemWide(), Float(x), Float(y), &hit) == .success,
        let hit, AXUIElementGetPid(hit, &pid) == .success, pid == app.processIdentifier else {
    throw HelperError.commandFailed("the point belongs to a different app or cannot be read", details: [
      "reason": "app_point_owner_mismatch", "expectedPid": String(app.processIdentifier), "actualPid": String(pid)
    ])
  }
}
