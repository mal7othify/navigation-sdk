import Combine
import CoreLocation
import Foundation

/// Idiomatic wrapper over the UniFFI `Navigator`.
///
/// Calls into Rust run on a private serial queue; results are published on
/// the main actor through `state`. Rejected fixes (stale, implausible,
/// invalid) land in `lastError` and leave `state` untouched.
@MainActor
public final class NavigationSession: ObservableObject {
    /// Latest trip state, `nil` until the first fix is accepted.
    @Published public private(set) var state: TripState?
    /// Most recent error from the core, if any.
    @Published public private(set) var lastError: NavError?
    /// Number of reroute requests the core has issued so far.
    @Published public private(set) var rerouteRequests: Int = 0

    private let navigator: Navigator
    private let queue = DispatchQueue(label: "com.navsdk.core", qos: .userInitiated)

    public init(route: Route, config: NavigatorConfig = NavigatorConfig()) throws {
        navigator = try Navigator(route: route, config: config)
    }

    /// Fire-and-forget update from a platform location.
    public func update(_ location: CLLocation) {
        update(RawLocation(location))
    }

    /// Fire-and-forget update; the result arrives on `state`.
    public func update(_ raw: RawLocation) {
        let navigator = self.navigator
        queue.async { [weak self] in
            let result = Result { try navigator.updateLocation(raw: raw) }
            Task { @MainActor [weak self] in
                self?.publish(result)
            }
        }
    }

    /// Update and return the resulting state, or throw `NavError`.
    public func updateAndWait(_ raw: RawLocation) async throws -> TripState {
        let navigator = self.navigator
        let queue = self.queue
        let result: Result<TripState, Error> = await withCheckedContinuation { continuation in
            queue.async {
                continuation.resume(returning: Result { try navigator.updateLocation(raw: raw) })
            }
        }
        publish(result)
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
    }

    private func publish(_ result: Result<TripState, Error>) {
        switch result {
        case .success(let next):
            if next.needsReroute { rerouteRequests += 1 }
            state = next
        case .failure(let error):
            lastError = error as? NavError
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
