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

/// A route in both binding layers' types, so one feed can drive both sessions.
struct DualRoute {
    let pull: NavSDK.Route
    let push: NavCorePush.Route

    /// Geometry as plain coordinates, for the schematic map.
    var points: [CLLocationCoordinate2D] {
        pull.geometry.map { CLLocationCoordinate2D(latitude: $0.lat, longitude: $0.lng) }
    }
    var stepRanges: [ClosedRange<Int>] {
        pull.steps.map { Int($0.startIndex)...Int($0.endIndex) }
    }
    var destination: NavSDK.GeoPoint { pull.geometry[pull.geometry.count - 1] }
}

private let replanLatencyNs: UInt64 = 300_000_000
private let divergenceThresholdM = 0.5

/// Drives one or two sessions over the same fixes. Pull and push sessions are
/// always created together so the "Both" tab can be switched to without
/// losing state. Metrics come straight from the wrappers' published counters.
///
/// Device mode can lay out a walking course at the phone's position
/// (`walkCourse`) so the navigator can be tested on foot with no map service;
/// while a course is active, each reroute request is answered with a straight
/// `directRoute` back to the destination (host-driven for pull, core-driven
/// for push). On the placeholder fixture route the device is nowhere near the
/// route, so no reroute is attempted.
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

    /// Route the sessions are navigating (the last installed one), for the schematic map.
    @Published private(set) var activeRoute: DualRoute?
    /// Most recent raw fix handed to the core (replayed, simulated or from the device).
    @Published private(set) var lastRawPoint: CLLocationCoordinate2D?
    /// Most recent device fix, for anchoring a walking course.
    @Published private(set) var lastDeviceLocation: CLLocation?
    /// Leg length of the walking course, metres.
    @Published var courseLegM: Double = 50
    /// Answer reroute requests with a straight line to the destination (walking course only).
    @Published var autoReroute = true
    /// True while the sessions navigate a walking course started from the device position.
    @Published private(set) var courseActive = false

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
        locationManager.distanceFilter = kCLDistanceFilterNone
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
            try startSessions(DualRoute(pull: a.route, push: b.route), course: false)
        } catch {
            loadError = "\(error)"
        }
    }

    /// Fresh sessions for both approaches on `route`.
    private func startSessions(_ route: DualRoute, course: Bool) throws {
        simulator = try NavCorePush.RouteSimulator(route: route.push, config: NavCorePush.SimulatorConfig())
        simulatorDeviating = false
        everDiverged = false
        courseActive = course
        activeRoute = route
        lastRawPoint = nil
        loadError = nil
        // Pull: host-driven. The wrapper runs this closure and installs the result.
        let pullSession = try NavigationSession(route: route.pull) { [weak self] request in
            try await Task.sleep(nanoseconds: replanLatencyNs)
            return await self?.replanPull(route, request)
        }
        // Push: core-driven. Rust calls this from its helper thread and installs the result itself.
        let pushSession = try NavigationSessionPush(route: route.push) { [weak self] request in
            try? await Task.sleep(nanoseconds: replanLatencyNs)
            return await self?.replanPush(route, request)
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

    /// The demo has no routing backend. Replaying or simulating a fixture, the
    /// "fetched" route is the same route. On a walking course it is a straight
    /// line from the device to the destination. On a fixture route in Device
    /// mode the phone is nowhere near it: decline.
    private func replanPull(_ route: DualRoute, _ request: NavSDK.RerouteRequest) -> NavSDK.Route? {
        if source != .device { return route.pull }
        guard autoReroute, courseActive else { return nil }
        do {
            let installed = try NavSDK.directRoute(from: request.origin, to: route.destination)
            activeRoute = DualRoute(pull: installed, push: Self.toPush(installed))
            return installed
        } catch {
            loadError = "reroute failed: \(error)"
            return nil
        }
    }

    private func replanPush(_ route: DualRoute, _ request: NavCorePush.RerouteRequest) -> NavCorePush.Route? {
        if source != .device { return route.push }
        guard autoReroute, courseActive else { return nil }
        do {
            let to = NavCorePush.GeoPoint(lat: route.destination.lat, lng: route.destination.lng)
            let installed = try NavCorePush.directRoute(from: request.origin, to: to)
            activeRoute = DualRoute(pull: Self.toPull(installed), push: installed)
            return installed
        } catch {
            loadError = "reroute failed: \(error)"
            return nil
        }
    }

    /// Lay out a zigzag walking course at the current device position, facing
    /// the way the device is moving (north if it has no course yet), and
    /// navigate it with both sessions.
    func startWalkCourse() {
        guard let here = lastDeviceLocation else { return }
        let heading = here.course >= 0 ? here.course : 0
        stop()
        do {
            let a = try NavSDK.walkCourse(
                origin: NavSDK.GeoPoint(lat: here.coordinate.latitude, lng: here.coordinate.longitude),
                headingDeg: heading, legM: courseLegM
            )
            let b = try NavCorePush.walkCourse(
                origin: NavCorePush.GeoPoint(lat: here.coordinate.latitude, lng: here.coordinate.longitude),
                headingDeg: heading, legM: courseLegM
            )
            try startSessions(DualRoute(pull: a, push: b), course: true)
        } catch {
            loadError = "\(error)"
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
        guard let a = pullFixture, let b = pushFixture else { return }
        stop()
        try? startSessions(DualRoute(pull: a.route, push: b.route), course: false)
        running = true
        let tab = approach
        let interval = UInt64(1_000_000_000 / speed)
        feedTask = Task { [weak self] in
            guard let self else { return }
            switch self.source {
            case .replay:
                for i in 0..<a.trace.count {
                    guard !Task.isCancelled else { return }
                    let raw = a.trace[i]
                    self.lastRawPoint = CLLocationCoordinate2D(latitude: raw.point.lat, longitude: raw.point.lng)
                    await self.feedOne(tab, raw, b.trace[i])
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
                    self.lastRawPoint = CLLocationCoordinate2D(latitude: fix.point.lat, longitude: fix.point.lng)
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
        if let a = pullFixture, let b = pushFixture {
            try? startSessions(DualRoute(pull: a.route, push: b.route), course: false)
        }
        switch next {
        case .device:
            locationManager.requestWhenInUseAuthorization()
            locationManager.startUpdatingLocation()
        case .replay, .simulator:
            locationManager.stopUpdatingLocation()
        }
    }

    /// The same route in the other binding layer's type.
    static func toPush(_ r: NavSDK.Route) -> NavCorePush.Route {
        NavCorePush.Route(
            geometry: r.geometry.map { NavCorePush.GeoPoint(lat: $0.lat, lng: $0.lng) },
            steps: r.steps.map {
                NavCorePush.RouteStep(
                    instruction: $0.instruction,
                    maneuver: Self.toPush($0.maneuver),
                    startIndex: $0.startIndex, endIndex: $0.endIndex, distanceM: $0.distanceM
                )
            }
        )
    }

    static func toPull(_ r: NavCorePush.Route) -> NavSDK.Route {
        NavSDK.Route(
            geometry: r.geometry.map { NavSDK.GeoPoint(lat: $0.lat, lng: $0.lng) },
            steps: r.steps.map {
                NavSDK.RouteStep(
                    instruction: $0.instruction,
                    maneuver: Self.toPull($0.maneuver),
                    startIndex: $0.startIndex, endIndex: $0.endIndex, distanceM: $0.distanceM
                )
            }
        )
    }
}

extension DemoModel {
    /// The UniFFI enum has no raw values; map case by case.
    static func toPush(_ m: NavSDK.ManeuverType) -> NavCorePush.ManeuverType {
        switch m {
        case .depart: return .depart
        case .turnLeft: return .turnLeft
        case .turnRight: return .turnRight
        case .slightLeft: return .slightLeft
        case .slightRight: return .slightRight
        case .straight: return .straight
        case .uTurn: return .uTurn
        case .arrive: return .arrive
        }
    }

    static func toPull(_ m: NavCorePush.ManeuverType) -> NavSDK.ManeuverType {
        switch m {
        case .depart: return .depart
        case .turnLeft: return .turnLeft
        case .turnRight: return .turnRight
        case .slightLeft: return .slightLeft
        case .slightRight: return .slightRight
        case .straight: return .straight
        case .uTurn: return .uTurn
        case .arrive: return .arrive
        }
    }
}

extension DemoModel: CLLocationManagerDelegate {
    nonisolated func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let last = locations.last else { return }
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.lastDeviceLocation = last
            self.lastRawPoint = last.coordinate
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
