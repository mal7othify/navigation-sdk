import Combine
import CoreLocation
import Foundation

/// Approach A, **pull**: idiomatic wrapper over the UniFFI `Navigator`.
///
/// Every fix is a synchronous call into Rust on a private serial queue that
/// returns the whole `FullState`. The wrapper splits it into `position`
/// (every fix) and `trip` (only when the core says it changed), published on
/// the main actor, so views observing `trip` re-render a handful of times per
/// drive.
///
/// Rerouting is host-driven: the core reports a `RerouteRequest` inside the
/// trip state, this wrapper records it in `lastReroute` and, when a
/// `replanner` is supplied, fetches the new route and installs it with
/// `setRoute`. Rejected fixes (stale, implausible, invalid) land in
/// `lastError` and leave the state untouched.
@MainActor
public final class NavigationSession: ObservableObject {
    /// Host-supplied route fetcher. Return `nil` to leave the current route.
    public typealias Replanner = @Sendable (RerouteRequest) async throws -> Route?

    /// Latest full state, `nil` until the first fix is accepted.
    @Published public private(set) var state: FullState?
    /// Per-fix state; changes on every accepted fix.
    @Published public private(set) var position: PositionState?
    /// Slow-changing state; changes only when the core reports a change.
    @Published public private(set) var trip: TripState?
    /// Most recent error from the core, if any.
    @Published public private(set) var lastError: NavError?
    /// Most recent reroute request (including a final give-up notice).
    @Published public private(set) var lastReroute: RerouteRequest?
    /// Number of actionable (`.offRoute`) reroute requests issued so far.
    @Published public private(set) var rerouteRequests: Int = 0
    /// Number of routes installed through `setRoute` (by the host or the replanner).
    @Published public private(set) var routesInstalled: Int = 0
    /// Accepted fixes so far (one `position` change each).
    @Published public private(set) var positionUpdates: Int = 0
    /// `trip` changes so far: only the updates the core flagged as changed.
    @Published public private(set) var tripUpdates: Int = 0
    /// Wall time of the most recent `updateLocation` boundary crossings, nanoseconds (newest last, at most 1024).
    @Published public private(set) var boundarySamplesNs: [UInt64] = []

    private let navigator: Navigator
    private let replanner: Replanner?
    private let queue = DispatchQueue(label: "com.navsdk.core", qos: .userInitiated)

    public init(route: Route, config: NavigatorConfig = NavigatorConfig(), replanner: Replanner? = nil) throws {
        navigator = try Navigator(route: route, config: config)
        self.replanner = replanner
    }

    /// Fire-and-forget update from a platform location.
    public func update(_ location: CLLocation) {
        update(RawLocation(location))
    }

    /// Fire-and-forget update; the result arrives on `position` / `trip`.
    public func update(_ raw: RawLocation) {
        let navigator = self.navigator
        queue.async { [weak self] in
            let start = DispatchTime.now().uptimeNanoseconds
            let result = Result { try navigator.updateLocation(raw: raw) }
            let elapsed = DispatchTime.now().uptimeNanoseconds - start
            Task { @MainActor [weak self] in
                self?.publish(result, elapsedNs: elapsed)
            }
        }
    }

    /// Update and return the resulting state, or throw `NavError`.
    public func updateAndWait(_ raw: RawLocation) async throws -> FullState {
        let navigator = self.navigator
        let queue = self.queue
        let (result, elapsed): (Result<FullState, Error>, UInt64) = await withCheckedContinuation { continuation in
            queue.async {
                let start = DispatchTime.now().uptimeNanoseconds
                let result = Result { try navigator.updateLocation(raw: raw) }
                continuation.resume(returning: (result, DispatchTime.now().uptimeNanoseconds - start))
            }
        }
        publish(result, elapsedNs: elapsed)
        return try result.get()
    }

    /// Install a new route after rerouting. Progress restarts on the new route.
    public func setRoute(_ route: Route) async throws {
        let navigator = self.navigator
        let queue = self.queue
        let result: Result<Void, Error> = await withCheckedContinuation { continuation in
            queue.async {
                continuation.resume(returning: Result { try navigator.setRoute(route: route) })
            }
        }
        try result.get()
        state = nil
        position = nil
        trip = nil
        routesInstalled += 1
    }

    private func publish(_ result: Result<FullState, Error>, elapsedNs: UInt64) {
        boundarySamplesNs.append(elapsedNs)
        if boundarySamplesNs.count > 1024 { boundarySamplesNs.removeFirst() }
        switch result {
        case .success(let next):
            position = next.position
            positionUpdates += 1
            if next.tripChanged {
                trip = next.trip
                tripUpdates += 1
            }
            state = next
            if let request = next.trip.reroute { onReroute(request) }
        case .failure(let error):
            lastError = error as? NavError
        }
    }

    private func onReroute(_ request: RerouteRequest) {
        lastReroute = request
        guard request.reason == .offRoute else { return }
        rerouteRequests += 1
        guard let replanner else { return }
        Task { [weak self] in
            guard let route = try? await replanner(request) else { return }
            try? await self?.setRoute(route)
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
