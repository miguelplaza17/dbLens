import Observation
import Sparkle

/// Updates through Sparkle: once a day it reads the appcast on GitHub Releases
/// (SUFeedURL in Info.plist), and an update is installed only when its
/// Ed25519 signature matches SUPublicEDKey. With "Update automatically" on (the
/// default) it downloads in the background, shows a notice in the window, and
/// installs when the app quits, or at once from the notice. Never in tests,
/// snapshots, or `dbLens --mcp`.
@Observable
final class Updater {
    /// The notice at the bottom of the window.
    enum Notice: Equatable {
        case downloading(String)
        case ready(String)
    }

    private(set) var notice: Notice?
    private(set) var lastCheck: Date?
    @ObservationIgnored private let controller: SPUStandardUpdaterController?
    @ObservationIgnored private let events = UpdaterEvents()
    @ObservationIgnored private var installNowBlock: (() -> Void)?

    init() {
        let enabled = !Store.isOverridden && !MCPServer.requested && Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") != nil
        controller = enabled ? SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: events, userDriverDelegate: nil) : nil
        automaticallyChecks = controller?.updater.automaticallyChecksForUpdates ?? false
        automaticallyUpdates = controller?.updater.automaticallyDownloadsUpdates ?? false
        lastCheck = controller?.updater.lastUpdateCheckDate
        events.owner = self
    }

    var isEnabled: Bool { controller != nil }

    /// Sparkle's daily check, on or off (Settings > Updates).
    var automaticallyChecks: Bool {
        didSet { controller?.updater.automaticallyChecksForUpdates = automaticallyChecks }
    }

    /// Download and install without asking (on by default, SUAutomaticallyUpdate).
    var automaticallyUpdates: Bool {
        didSet { controller?.updater.automaticallyDownloadsUpdates = automaticallyUpdates }
    }

    /// Sparkle shows the check already running, if there is one.
    func checkForUpdates() {
        controller?.checkForUpdates(nil)
        lastCheck = .now
    }

    /// From the notice: quit, install, and open the new version.
    func installNow() { installNowBlock?() }

    func dismissNotice() { notice = nil }

    /// Snapshot mode: the notice as it looks, without an update.
    func showForSnapshot(_ notice: Notice) { self.notice = notice }

    fileprivate func found(_ version: String) {
        if automaticallyUpdates { notice = .downloading(version) }
    }

    fileprivate func ready(_ version: String, install: @escaping () -> Void) {
        installNowBlock = install
        notice = .ready(version)
    }

    fileprivate func stopped() {
        if case .downloading = notice { notice = nil }
    }
}

/// Sparkle's callbacks, passed on to the Updater (always on the main thread).
private final class UpdaterEvents: NSObject, SPUUpdaterDelegate {
    weak var owner: Updater?

    func updater(_ updater: SPUUpdater, didFindValidUpdate item: SUAppcastItem) {
        owner?.found(item.displayVersionString)
    }

    /// The short update window, without the embedded release notes page.
    func updater(_ updater: SPUUpdater, shouldDownloadReleaseNotesForUpdate updateItem: SUAppcastItem) -> Bool { false }

    func updater(_ updater: SPUUpdater, willInstallUpdateOnQuit item: SUAppcastItem,
                 immediateInstallationBlock immediateInstallHandler: @escaping () -> Void) -> Bool {
        owner?.ready(item.displayVersionString, install: immediateInstallHandler)
        return true
    }

    func updater(_ updater: SPUUpdater, didAbortWithError error: Error) {
        owner?.stopped()
    }
}
