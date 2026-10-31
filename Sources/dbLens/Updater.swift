import Sparkle

/// Updates through Sparkle: once a day it reads the appcast on GitHub Releases
/// (SUFeedURL in Info.plist), and an update is installed only when its
/// Ed25519 signature matches SUPublicEDKey. Test and snapshot runs never check.
final class Updater {
    private let controller: SPUStandardUpdaterController?

    init() {
        let enabled = !Store.isOverridden && Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") != nil
        controller = enabled ? SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: nil, userDriverDelegate: nil) : nil
    }

    var isEnabled: Bool { controller != nil }

    /// Sparkle shows the check already running, if there is one.
    func checkForUpdates() {
        controller?.checkForUpdates(nil)
    }
}
