import Combine
import CoreLocation
import Foundation

/// A rejected fix or failed route install reported by the core.
public struct NavErrorEvent: Equatable, Sendable {
    public let kind: ErrorKind
    public let message: String
}

/// Approach B, **push**: idiomatic wrapper over the BoltFFI `NavigationController`.
///
/// Fixes are handed to Rust on a private serial queue and Rust calls back
/// through a `NavigationListener` on that same queue: a `PositionState` for
/// every accepted fix, a `TripState` only when it changed, and a
/// `SessionState` on lifecycle events. The callbacks are republished on the
/// main actor, so nothing is polled and nothing is returned across the
/// boundary.
///
/// Rerouting is core-owned: when a `replanner` is supplied the controller
/// calls it from a Rust helper thread, installs the returned route itself and
/// reports the outcome through `session`. Without one, requests are only
/// visible in `trip`.
@MainActor
public final class NavigationSessionPush: ObservableObject {
    /// Host-supplied route fetcher. Return `nil` to decline.
    public typealias Replanner = @Sendable (RerouteRequest) async -> Route?

    /// Per-fix state; changes on every accepted fix.
    @Published public private(set) var position: PositionState?
    /// Slow-changing state; changes only when the core sends it.
    @Published public private(set) var trip: TripState?
    /// Lifecycle: routes installed, replans in flight / succeeded / failed.
    @Published public private(set) var session: SessionState
    /// Most recent error from the core, if any.
    @Published public private(set) var lastError: NavErrorEvent?
    /// Most recent reroute request (including a final give-up notice).
    @Published public private(set) var lastReroute: RerouteRequest?
    /// Number of actionable (`.offRoute`) reroute requests issued so far.
    @Published public private(set) var rerouteRequests: Int = 0
    /// How many `onPosition` callbacks crossed the boundary.
    @Published public private(set) var positionCallbacks: Int = 0
    /// How many `onTrip` callbacks crossed the boundary.
    @Published public private(set) var tripCallbacks: Int = 0
    /// Wall time of the most recent `updateLocation` boundary crossings including the
    /// callbacks Rust makes before returning, nanoseconds (newest last, at most 1024).
    @Published public private(set) var boundarySamplesNs: [UInt64] = []

    private let controller: NavigationController
    private let queue = DispatchQueue(label: "com.navsdk.push", qos: .userInitiated)
    private var listener: Listener?
    private var listenerId: UInt32 = 0
    private var replannerBox: ReplannerBox?

    public init(route: Route, config: NavigatorConfig = NavigatorConfig(), replanner: Replanner? = nil) throws {
        controller = try NavigationController(route: route, config: config)
        session = controller.session()
        let listener = Listener(owner: self)
        self.listener = listener
        listenerId = controller.addListener(listener: listener)
        if let replanner {
            let box = ReplannerBox(replanner)
            replannerBox = box
            controller.setReplanner(replanner: box)
        }
    }

    deinit {
        // `controller` releases its Rust object when the last Swift reference
        // goes away; the listener registration dies with it.
    }

    /// Fire-and-forget update from a platform location.
    public func update(_ location: CLLocation) {
        update(RawLocation(location))
    }

    /// Fire-and-forget update; results arrive on `position` / `trip`.
    public func update(_ raw: RawLocation) {
        let controller = self.controller
        queue.async { [weak self] in
            let start = DispatchTime.now().uptimeNanoseconds
            controller.updateLocation(raw: raw)
            let elapsed = DispatchTime.now().uptimeNanoseconds - start
            Task { @MainActor [weak self] in self?.apply(sampleNs: elapsed) }
        }
    }

    /// Hand several fixes to Rust in one boundary crossing, in order.
    public func updateBatch(_ fixes: [RawLocation]) {
        let controller = self.controller
        queue.async { controller.updateLocationBatch(fixes: fixes) }
    }

    /// Update and wait until the core has called back. Returns the position
    /// after this fix, or `nil` if the fix was rejected (see `lastError`).
    public func updateAndWait(_ raw: RawLocation) async -> PositionState? {
        let controller = self.controller
        let queue = self.queue
        let listener = self.listener
        let before = positionCallbacks
        let elapsed: UInt64 = await withCheckedContinuation { continuation in
            queue.async {
                let start = DispatchTime.now().uptimeNanoseconds
                controller.updateLocation(raw: raw)
                continuation.resume(returning: DispatchTime.now().uptimeNanoseconds - start)
            }
        }
        apply(sampleNs: elapsed)
        // Callbacks were queued onto the main actor from the serial queue;
        // drain them before reading.
        await listener?.drain()
        return positionCallbacks == before ? nil : position
    }

    /// Install a new route by hand. Progress restarts on the new route.
    public func setRoute(_ route: Route) async throws {
        let controller = self.controller
        let queue = self.queue
        let result: Result<Void, Error> = await withCheckedContinuation { continuation in
            queue.async {
                continuation.resume(returning: Result { try controller.setRoute(route: route) })
            }
        }
        try result.get()
        position = nil
        trip = nil
    }

    /// Stop accepting fixes until the next `setRoute`.
    public func stop() {
        let controller = self.controller
        queue.async { controller.stop() }
    }

    /// Wait until every callback already delivered by Rust has been applied.
    public func settle() async {
        await listener?.drain()
    }

    // MARK: - Callback plumbing

    fileprivate func apply(position: PositionState) {
        self.position = position
        positionCallbacks += 1
    }

    fileprivate func apply(trip: TripState) {
        self.trip = trip
        tripCallbacks += 1
        if let request = trip.reroute {
            lastReroute = request
            if request.reason == .offRoute { rerouteRequests += 1 }
        }
    }

    fileprivate func apply(session: SessionState) {
        self.session = session
    }

    fileprivate func apply(error: NavErrorEvent) {
        lastError = error
    }

    fileprivate func apply(sampleNs: UInt64) {
        boundarySamplesNs.append(sampleNs)
        if boundarySamplesNs.count > 1024 { boundarySamplesNs.removeFirst() }
    }

    /// Receives Rust callbacks on whatever thread fed the fix (the serial
    /// queue, or the replanning helper thread for session updates) and hops
    /// them onto the main actor in order.
    private final class Listener: NavigationListener, @unchecked Sendable {
        weak var owner: NavigationSessionPush?
        private let pending = DispatchGroup()

        init(owner: NavigationSessionPush) {
            self.owner = owner
        }

        private func hop(_ body: @escaping @MainActor (NavigationSessionPush) -> Void) {
            pending.enter()
            let pending = self.pending
            Task { @MainActor [weak owner] in
                if let owner { body(owner) }
                pending.leave()
            }
        }

        func drain() async {
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                pending.notify(queue: .main) { continuation.resume() }
            }
        }

        func onPosition(position: PositionState) {
            hop { $0.apply(position: position) }
        }

        func onTrip(trip: TripState) {
            hop { $0.apply(trip: trip) }
        }

        func onSession(session: SessionState) {
            hop { $0.apply(session: session) }
        }

        func onError(kind: ErrorKind, message: String) {
            hop { $0.apply(error: NavErrorEvent(kind: kind, message: message)) }
        }
    }

    private final class ReplannerBox: ReplanningService, @unchecked Sendable {
        let replanner: Replanner
        init(_ replanner: @escaping Replanner) {
            self.replanner = replanner
        }
        func replan(request: RerouteRequest) async -> Route? {
            await replanner(request)
        }
    }
}

extension RawLocation {
    /// Convert a Core Location fix. Negative accuracy/speed/course mean
    /// "unknown" on Apple platforms and become `nil`.
    public init(_ location: CLLocation) {
        self.init(
            point: GeoPoint(lat: location.coordinate.latitude, lng: location.coordinate.longitude),
            accuracyM: location.horizontalAccuracy >= 0 ? location.horizontalAccuracy : nil,
            speedMps: location.speed >= 0 ? location.speed : nil,
            courseDeg: location.course >= 0 ? location.course : nil,
            timestampMs: UInt64(max(0, location.timestamp.timeIntervalSince1970 * 1000))
        )
    }
}
