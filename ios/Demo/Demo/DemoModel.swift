import Combine
import CoreLocation
import Foundation
import NavSDK

enum Source: String, CaseIterable, Identifiable {
    case replay = "Replay"
    case device = "Device"
    var id: String { rawValue }
}

/// Drives the demo: loads a fixture, replays its trace at 1 Hz (or faster), or
/// forwards real Core Location fixes, and exposes the session to the UI.
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

    private var fixture: Fixture?
    private var replayTask: Task<Void, Never>?
    private let locationManager = CLLocationManager()

    override init() {
        super.init()
        locationManager.delegate = self
        locationManager.desiredAccuracy = kCLLocationAccuracyBest
        load()
    }

    private func load() {
        stopReplay()
        do {
            let loaded = try Fixture(name: fixtureName, bundle: .main)
            fixture = loaded
            replayTotal = loaded.trace.count
            replayIndex = 0
            stepCount = loaded.route.steps.count
            loadError = nil
            try resetSession(loaded)
        } catch {
            loadError = "\(error)"
        }
    }

    private func resetSession(_ loaded: Fixture) throws {
        session = try NavigationSession(route: loaded.route)
    }

    func startReplay(speed: Double = 1.0) {
        guard let loaded = fixture else { return }
        stopReplay()
        try? resetSession(loaded)
        replaying = true
        replayTask = Task { [weak self] in
            for (i, raw) in loaded.trace.enumerated() {
                guard !Task.isCancelled, let self else { return }
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

    func setSource(_ next: Source) {
        guard next != source else { return }
        source = next
        stopReplay()
        if let loaded = fixture { try? resetSession(loaded) }
        switch next {
        case .device:
            locationManager.requestWhenInUseAuthorization()
            locationManager.startUpdatingLocation()
        case .replay:
            locationManager.stopUpdatingLocation()
        }
    }
}

extension DemoModel: CLLocationManagerDelegate {
    nonisolated func locationManager(_ manager: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let last = locations.last else { return }
        Task { @MainActor [weak self] in
            self?.session?.update(last)
        }
    }
}
