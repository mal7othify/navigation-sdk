import Combine
import CoreLocation
import Foundation
import NavCorePush
import NavSDK

/// Which implementation the screen is driving.
enum Approach: String, CaseIterable, Identifiable {
    case pull = "Pull · UniFFI"
    case push = "Push · BoltFFI"
    case both = "Both"
    var id: String { rawValue }
}

/// Where fixes come from.
enum Source: String, CaseIterable, Identifiable {
    case replay = "Replay"
    case simulator = "Simulator"
    case device = "Device"
    var id: String { rawValue }
}

private let replanLatencyNs: UInt64 = 300_000_000
private let divergenceThresholdM = 0.5

/// Drives one or two sessions over the same fixes. Pull and push sessions are
/// always created together so the "Both" tab can be switched to without
/// losing state. Metrics come straight from the wrappers' published counters.
@MainActor
final class DemoModel: NSObject, ObservableObject {
    @Published var approach: Approach = .pull
    @Published private(set) var source: Source = .replay
    @Published var fixtureName = NavSDK.Fixture.names[0] { didSet { load() } }
    @Published private(set) var pull: NavigationSession?
    @Published private(set) var push: NavigationSessionPush?
    @Published private(set) var running = false
    @Published private(set) var feedIndex = 0
    @Published private(set) var feedTotal = 0
    @Published private(set) var loadError: String?
    @Published private(set) var simulatorDeviating = false
    @Published private(set) var everDiverged = false

    private var pullFixture: NavSDK.Fixture?
    private var pushFixture: NavCorePush.Fixture?
    private var simulator: NavCorePush.RouteSimulator?
    private var feedTask: Task<Void, Never>?
    private var divergenceWatch: AnyCancellable?
    private let locationManager = CLLocationManager()

    override init() {
        super.init()
        locationManager.delegate = self
        locationManager.desiredAccuracy = kCLLocationAccuracyBest
        load()
    }

    private func load() {
        stop()
        do {
            guard let url = Bundle.main.url(forResource: fixtureName, withExtension: "json", subdirectory: "Fixtures")
                ?? Bundle.main.url(forResource: fixtureName, withExtension: "json")
            else { throw NavSDK.Fixture.LoadError.malformed("\(fixtureName).json not in bundle") }
            let data = try Data(contentsOf: url)
            let a = try NavSDK.Fixture(name: fixtureName, data: data)
            let b = try NavCorePush.Fixture(name: fixtureName, data: data)
            pullFixture = a
            pushFixture = b
            feedTotal = a.trace.count
            feedIndex = 0
            loadError = nil
            try resetSessions()
        } catch {
            loadError = "\(error)"
        }
    }

    /// Fresh sessions for both approaches.
    private func resetSessions() throws {
        guard let a = pullFixture, let b = pushFixture else { return }
        let pullRoute = a.route
        let pushRoute = b.route
        simulator = try NavCorePush.RouteSimulator(route: pushRoute, config: NavCorePush.SimulatorConfig())
        simulatorDeviating = false
        everDiverged = false
        // Pull: host-driven rerouting. No routing backend in the demo, so the
        // "fetched" route is the same route after a short delay.
        let pullSession = try NavigationSession(route: pullRoute) { _ in
            try await Task.sleep(nanoseconds: replanLatencyNs)
            return pullRoute
        }
        // Push: the core calls this from its helper thread and installs the result itself.
        let pushSession = try NavigationSessionPush(route: pushRoute) { _ in
            try? await Task.sleep(nanoseconds: replanLatencyNs)
            return pushRoute
        }
        pull = pullSession
        push = pushSession
        divergenceWatch = pullSession.$position
            .combineLatest(pushSession.$position)
            .sink { [weak self] a, b in
                guard let self, let d = Self.divergence(a, b), d > divergenceThresholdM else { return }
                self.everDiverged = true
            }
    }

    /// Metres between the two snapped positions, when both exist.
    static func divergence(_ a: NavSDK.PositionState?, _ b: NavCorePush.PositionState?) -> Double? {
        guard let a, let b else { return nil }
        let p = CLLocation(latitude: a.snapped.point.lat, longitude: a.snapped.point.lng)
        let q = CLLocation(latitude: b.snapped.point.lat, longitude: b.snapped.point.lng)
        return p.distance(from: q)
    }

    /// Feed the chosen source at 1 Hz × `speed` to the session(s) of the current tab.
    func start(speed: Double = 1.0) {
        guard pullFixture != nil else { return }
        stop()
        try? resetSessions()
        running = true
        let tab = approach
        let interval = UInt64(1_000_000_000 / speed)
        feedTask = Task { [weak self] in
            guard let self else { return }
            switch self.source {
            case .replay:
                guard let a = self.pullFixture, let b = self.pushFixture else { return }
                for i in 0..<a.trace.count {
                    guard !Task.isCancelled else { return }
                    await self.feedOne(tab, a.trace[i], b.trace[i])
                    self.feedIndex = i + 1
                    try? await Task.sleep(nanoseconds: interval)
                }
            case .simulator:
                guard let sim = self.simulator else { return }
                self.feedTotal = 0
                var n = 0
                var tail = 5
                while tail > 0, !Task.isCancelled {
                    let fix = sim.nextFix()
                    if tab != .push { self.pull?.update(fix.toPull()) }
                    if tab != .pull { self.push?.update(fix) }
                    n += 1
                    self.feedIndex = n
                    if sim.isFinished() { tail -= 1 }
                    try? await Task.sleep(nanoseconds: interval)
                }
            case .device:
                return
            }
            self.running = false
        }
    }

    private func feedOne(_ tab: Approach, _ a: NavSDK.RawLocation, _ b: NavCorePush.RawLocation) async {
        switch tab {
        case .pull: pull?.update(a)
        case .push: push?.update(b)
        case .both:
            // Lockstep: wait for each so the divergence check compares the same fix.
            _ = try? await pull?.updateAndWait(a)
            _ = await push?.updateAndWait(b)
        }
    }

    /// Simulator only: leave the route 50 m ahead for 300 m, 80 m to the right.
    func deviateNow() {
        guard let sim = simulator else { return }
        sim.setPlannedDeviation(deviation: NavCorePush.PlannedDeviation(
            atM: sim.distanceAlongM() + 50, lateralM: 80, lengthM: 300
        ))
        simulatorDeviating = true
    }

    func stop() {
        feedTask?.cancel()
        feedTask = nil
        running = false
    }

    func setSource(_ next: Source) {
        guard next != source else { return }
        source = next
        stop()
        try? resetSessions()
        switch next {
        case .device:
            locationManager.requestWhenInUseAuthorization()
            locationManager.startUpdatingLocation()
        case .replay, .simulator:
            locationManager.stopUpdatingLocation()
        }
    }
}

extension DemoModel: CLLocationManagerDelegate {
    nonisolated func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let last = locations.last else { return }
        Task { @MainActor [weak self] in
            guard let self else { return }
            if self.approach != .push { self.pull?.update(last) }
            if self.approach != .pull { self.push?.update(last) }
        }
    }
}

extension NavCorePush.RawLocation {
    /// The same fix in the pull module's type.
    func toPull() -> NavSDK.RawLocation {
        NavSDK.RawLocation(
            point: NavSDK.GeoPoint(lat: point.lat, lng: point.lng),
            accuracyM: accuracyM, speedMps: speedMps, courseDeg: courseDeg, timestampMs: timestampMs
        )
    }
}
