import Foundation
#if canImport(QuartzCore)
import QuartzCore
#endif

/// The iPad side of the protocol clock.
///
/// UITouch.timestamp uses the same time base as
/// `ProcessInfo.systemUptime` (seconds since boot, not counting sleep).
/// We never send raw uptime off the device (privacy manifest "required
/// reason" rules discourage it); timestamps are nanoseconds since app launch
/// instead. The daemon only needs a consistent clock, it estimates the
/// offset with Ping/Pong.
public enum SessionClock {
    public static let epoch: TimeInterval = ProcessInfo.processInfo.systemUptime

    /// Session nanoseconds for a UIKit timestamp (seconds since boot).
    public static func ns(fromUptime t: TimeInterval) -> UInt64 {
        let d = t - epoch
        return d <= 0 ? 0 : UInt64(d * 1_000_000_000)
    }

    public static func nowNs() -> UInt64 {
        ns(fromUptime: ProcessInfo.processInfo.systemUptime)
    }

    #if canImport(QuartzCore)
    /// Session nanoseconds for a Core Animation / Metal host time
    /// (`CACurrentMediaTime()`, `MTLDrawable.presentedTime`). Both clocks
    /// count mach_absolute_time on Apple platforms; the gap between them is
    /// measured rather than assumed to be zero.
    public static func ns(fromMediaTime t: CFTimeInterval) -> UInt64 {
        let gap = ProcessInfo.processInfo.systemUptime - CACurrentMediaTime()
        return ns(fromUptime: t + gap)
    }
    #endif
}
