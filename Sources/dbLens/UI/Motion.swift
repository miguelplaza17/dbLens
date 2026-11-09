import SwiftUI

/// Motion for the app's own feedback, like dragging in the sidebar.
/// Views take their animations and timings from here.
enum Motion {
    /// A drop target lighting up, or a slot opening where a row will land.
    static let hover = Animation.smooth(duration: 0.42)
    /// A dropped row settling into its new place.
    static let land = Animation.spring(duration: 0.45, bounce: 0.12)
    /// A folder opening or closing.
    static let disclose = Animation.smooth(duration: 0.28)
    /// How long a target stays lit after the pointer leaves it, so going on
    /// to the next row moves the highlight instead of blinking it off and on.
    static let leaveGrace = Duration.milliseconds(150)
    /// How long a closed folder waits under a drag before it opens.
    static let springLoad = Duration.milliseconds(600)
}
