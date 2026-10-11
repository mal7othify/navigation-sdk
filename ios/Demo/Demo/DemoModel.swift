import Combine
import CoreLocation
import Foundation
import NavSDK

enum Source: String, CaseIterable, Identifiable {
    case replay = "Replay"
    case device = "Device"
    var id: String { rawValue }
}

/// Drives the demo: loads a fixture and replays its trace, or forwards real
/// Core Location fixes along a walking course anchored at the device, and
/// exposes the session plus the active route (for the schematic map) to the UI.
@MainActor
final class DemoModel: NSObject, ObservableObject {
    @Published private(set) var session: NavigationSession?
    @Published var fixtureName = Fixture.names[0] { didSet { load() } }
    @Published private(set) var source: Source = .replay
    @Published private(set) var replaying = false
    @Published private(set) var replayIndex = 0
    @Published private(set) var replayTotal = 0
    @Published private(set) var stepCount = 0
    @Published private(set) var loadError: String?

    /// Route the session is navigating; drawn by the schematic map.
    @Published private(set) var activeRoute: Route?
    /// Most recent raw fix handed to the core (replayed or from the device).
    @Published private(set) var lastRawPoint: GeoPoint?
    /// Most recent device fix, for anchoring a walking course.
    @Published private(set) var lastDeviceLocation: CLLocation?
    /// Leg length of the walking course, metres.
    @Published var courseLegM: Double = 50
    /// When the core asks for a reroute, install a straight line to the destination.
    @Published var autoReroute = true
    @Published private(set) var reroutesInstalled = 0
    /// True while the session is navigating a walking course started from the device position.
    @Published private(set) var courseActive = false

    private var fixture: Fixture?
    private var replayTask: Task<Void, Never>?
    private var rerouteWatcher: AnyCancellable?
    private var destination: GeoPoint?
    private let locationManager = CLLocationManager()

    override init() {
        super.init()
        locationManager.delegate = self
        locationManager.desiredAccuracy = kCLLocationAccuracyBest
        locationManager.distanceFilter = kCLDistanceFilterNone
        load()
    }

    private func load() {
        stopReplay()
        do {
            let loaded = try Fixture(name: fixtureName, bundle: .main)
            fixture = loaded
            replayTotal = loaded.trace.count
            replayIndex = 0
            loadError = nil
            try startSession(route: loaded.route)
        } catch {
            loadError = "\(error)"
        }
    }

    /// Replace the session with one navigating `route`.
    private func startSession(route: Route, course: Bool = false) throws {
        rerouteWatcher = nil
        let next = try NavigationSession(route: route)
        session = next
        activeRoute = route
        stepCount = route.steps.count
        destination = route.geometry.last
        courseActive = course
        reroutesInstalled = 0
        lastRawPoint = nil
        loadError = nil
        // Each reroute request the core issues installs a straight-line route
        // from the current position to the original destination.
        rerouteWatcher = next.$rerouteRequests
            .dropFirst()
            .sink { [weak self] _ in
                guard let self else { return }
                Task { @MainActor in await self.handleRerouteRequest() }
            }
    }

    /// Only meaningful while walking a course: a fixture route in Device mode
    /// is just a placeholder and the device is nowhere near it.
    private func handleRerouteRequest() async {
        guard autoReroute, courseActive,
              let session, let destination,
              let here = lastDeviceLocation?.coordinate else { return }
        do {
            let from = GeoPoint(lat: here.latitude, lng: here.longitude)
            // Route building stays off the main actor.
            let route = try await Task.detached { try directRoute(from: from, to: destination) }.value
            guard session === self.session else { return } // replaced meanwhile
            try await session.setRoute(route)
            activeRoute = route
            stepCount = route.steps.count
            reroutesInstalled += 1
        } catch {
            loadError = "reroute failed: \(error)"
        }
    }

    // MARK: Replay

    func startReplay(speed: Double = 1.0) {
        guard let loaded = fixture else { return }
        stopReplay()
        try? startSession(route: loaded.route)
        replaying = true
        replayTask = Task { [weak self] in
            for (i, raw) in loaded.trace.enumerated() {
                guard !Task.isCancelled, let self else { return }
                self.lastRawPoint = raw.point
                self.session?.update(raw)
                self.replayIndex = i + 1
                try? await Task.sleep(nanoseconds: UInt64(1_000_000_000 / speed))
            }
            self?.replaying = false
        }
    }

    func stopReplay() {
        replayTask?.cancel()
        replayTask = nil
        replaying = false
    }

    // MARK: Device

    func setSource(_ next: Source) {
        guard next != source else { return }
        source = next
        stopReplay()
        if let loaded = fixture { try? startSession(route: loaded.route) }
        switch next {
        case .device:
            locationManager.requestWhenInUseAuthorization()
            locationManager.startUpdatingLocation()
        case .replay:
            locationManager.stopUpdatingLocation()
        }
    }

    /// Lay out a square walking course at the current device position, facing
    /// the way the device is moving (north if it has no course yet).
    func startWalkCourse() {
        guard let here = lastDeviceLocation else { return }
        let heading = here.course >= 0 ? here.course : 0
        do {
            let route = try walkCourse(
                origin: GeoPoint(lat: here.coordinate.latitude, lng: here.coordinate.longitude),
                headingDeg: heading,
                legM: courseLegM
            )
            try startSession(route: route, course: true)
        } catch {
            loadError = "\(error)"
        }
    }
}

extension DemoModel: CLLocationManagerDelegate {
    nonisolated func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let last = locations.last else { return }
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.lastDeviceLocation = last
            self.lastRawPoint = GeoPoint(lat: last.coordinate.latitude, lng: last.coordinate.longitude)
            self.session?.update(last)
        }
    }
}
