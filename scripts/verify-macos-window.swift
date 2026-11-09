import AppKit
import CoreGraphics
import Foundation

guard CommandLine.arguments.count == 2 else {
    fputs("Usage: swift verify-macos-window.swift /Applications/CapTerm.app\n", stderr)
    exit(1)
}
let installedApp = URL(fileURLWithPath: CommandLine.arguments[1]).resolvingSymlinksInPath()
guard let bundleID = Bundle(url: installedApp)?.bundleIdentifier else {
    fputs("Cannot read the bundle identifier at \(installedApp.path)\n", stderr)
    exit(1)
}

func mainWindow(for pid: pid_t, in windows: [[String: Any]]) -> (id: UInt32, bounds: CGRect)? {
    for window in windows {
        // Window titles can be omitted without Screen Recording permission.
        guard (window[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid,
              (window[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
              let id = (window[kCGWindowNumber as String] as? NSNumber)?.uint32Value,
              let dictionary = window[kCGWindowBounds as String] as? [String: Any],
              let bounds = CGRect(dictionaryRepresentation: dictionary as CFDictionary),
              bounds.width >= 500, bounds.height >= 460
        else { continue }
        return (id, bounds)
    }
    return nil
}

let process = ProcessInfo.processInfo
let deadline = process.systemUptime + 45
var visibleWindow: (id: UInt32, since: TimeInterval)?
var sawApp = false
while process.systemUptime < deadline {
    guard let windows = CGWindowListCopyWindowInfo(
        [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID
    ) as? [[String: Any]] else {
        fputs("Cannot inspect windows in this macOS GUI session\n", stderr)
        exit(1)
    }
    let app = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).first {
        $0.bundleURL?.resolvingSymlinksInPath().path == installedApp.path
    }
    sawApp = sawApp || app != nil
    if let app, let window = mainWindow(for: app.processIdentifier, in: windows) {
        let now = process.systemUptime
        if let visibleWindow, visibleWindow.id == window.id {
            if now - visibleWindow.since >= 2 {
                print("CapTerm main window visible for at least 2s from \(installedApp.path)")
                print("PID \(app.processIdentifier), window \(window.id), bounds \(window.bounds)")
                exit(0)
            }
        } else {
            visibleWindow = (window.id, now)
        }
    } else {
        visibleWindow = nil
    }
    Thread.sleep(forTimeInterval: 0.25)
}

fputs("No sustained CapTerm main window. Installed process observed: \(sawApp)\n", stderr)
exit(1)
