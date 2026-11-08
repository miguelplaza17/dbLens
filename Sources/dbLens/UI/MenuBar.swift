import AppKit
import SwiftUI

/// dbLens in the menu bar: closing the window leaves the app running (saved
/// connections, open SSH tunnels), and this menu brings the window back.
/// Settings > General turns it off.
enum MenuBar {
    static let key = "keepInMenuBar"

    /// Whether closing the last window keeps dbLens running. Tests, snapshots
    /// and the self-test (APP_STORE) always quit, so they never wait forever.
    static var keepsRunning: Bool {
        !Store.isOverridden && (UserDefaults.standard.object(forKey: key) as? Bool ?? true)
    }

    /// Back in the Dock and in front, for the window or Settings.
    @MainActor static func bringForward() {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate()
    }

    /// The main window, in front and out of the Dock if it was minimized.
    /// False when there is none (it was closed): the caller opens a new one.
    @MainActor @discardableResult static func showMainWindow() -> Bool {
        bringForward()
        // The main window: a titled window that is not Settings or a panel (minimized ones included).
        let main = NSApp.windows.filter {
            $0.styleMask.contains(.titled) && !($0 is NSPanel) && $0.identifier?.rawValue.localizedCaseInsensitiveContains("settings") != true
        }
        guard let window = main.first else { return false }
        if window.isMiniaturized { window.deminiaturize(nil) }
        window.makeKeyAndOrderFront(nil)
        return true
    }

    /// With no window left, the Dock icon goes and only the menu bar item stays.
    @MainActor static func hideIfNoWindows() {
        guard keepsRunning else { return }
        let open = NSApp.windows.contains { $0.isVisible && $0.canBecomeMain && !($0 is NSPanel) }
        if !open { NSApp.setActivationPolicy(.accessory) }
    }
}

struct MenuBarContent: View {
    @Environment(\.openWindow) private var openWindow
    @Environment(AppModel.self) private var model
    @Environment(Updater.self) private var updater

    var body: some View {
        Button("Open dbLens") {
            if !MenuBar.showMainWindow() { openWindow(id: "main") }
        }
        if let connection = model.activeConnection, model.session != nil {
            Text("Connected to \(connection.name)")
        }
        Divider()
        Button("Settings…") {
            MenuBar.bringForward()
            NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
        }
        Button("Check for Updates…") { updater.checkForUpdates() }
            .disabled(!updater.isEnabled)
        Divider()
        Button("Quit dbLens") { NSApp.terminate(nil) }
    }
}
