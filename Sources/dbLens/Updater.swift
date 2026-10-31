import Observation
import Sparkle

/// Updates through Sparkle: once a day it reads the appcast on GitHub Releases
/// (SUFeedURL in Info.plist), and an update is installed only when its
/// Ed25519 signature matches SUPublicEDKey. Test and snapshot runs never check.
@Observable
final class Updater {
    @ObservationIgnored private let controller: SPUStandardUpdaterController?

    init() {
        let enabled = !Store.isOverridden && Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") != nil
        controller = enabled ? SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: nil, userDriverDelegate: nil) : nil
        automaticallyChecks = controller?.updater.automaticallyChecksForUpdates ?? false
        lastCheck = controller?.updater.lastUpdateCheckDate
    }

    var isEnabled: Bool { controller != nil }

    /// Sparkle's daily check, on or off (Settings > Updates).
    var automaticallyChecks: Bool {
        didSet { controller?.updater.automaticallyChecksForUpdates = automaticallyChecks }
    }

    private(set) var lastCheck: Date?

    /// Sparkle shows the check already running, if there is one.
    func checkForUpdates() {
        controller?.checkForUpdates(nil)
        lastCheck = .now
    }
}
