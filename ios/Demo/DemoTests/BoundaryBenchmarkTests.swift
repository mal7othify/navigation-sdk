import NavCorePush
import NavSDK
import XCTest

/// Boundary-cost microbenchmark, Phase 14: the same simulated drive through
/// the pull (UniFFI) and push (BoltFFI) bindings in Swift.
///
/// Not a unit test: it prints a table (grep `BOUNDARY BENCH` in the
/// xcodebuild log). Run with `scripts/bench-boundary.sh`.
final class BoundaryBenchmarkTests: XCTestCase {
    private let calls = 10_000

    private struct Stats {
        let avgUs: Double, p50Us: Double, p99Us: Double, maxUs: Double
        init(_ samples: [UInt64]) {
            let sorted = samples.sorted()
            avgUs = Double(sorted.reduce(0, +)) / Double(sorted.count) / 1000
            p50Us = Double(sorted[sorted.count / 2]) / 1000
            p99Us = Double(sorted[Int(Double(sorted.count - 1) * 0.99)]) / 1000
            maxUs = Double(sorted.last ?? 0) / 1000
        }
    }

    private func fixtureRouteJson() throws -> String {
        let bundle = Bundle(for: BoundaryBenchmarkTests.self)
        let appBundle = Bundle.allBundles.first { $0.bundleIdentifier == "com.navsdk.demo" } ?? bundle
        guard let url = appBundle.url(forResource: "route_simple", withExtension: "json", subdirectory: "Fixtures")
            ?? appBundle.url(forResource: "route_simple", withExtension: "json")
        else { throw NavSDK.Fixture.LoadError.malformed("route_simple.json not in bundle") }
        let root = try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any]
        let route = try JSONSerialization.data(withJSONObject: root?["route"] ?? [:])
        return String(decoding: route, as: UTF8.self)
    }

    /// A long straight route with `vertices` points, to show record-size sensitivity.
    private func longRouteJson(vertices: Int) -> String {
        var geometry: [String] = []
        for i in 0..<vertices { geometry.append("{\"lat\":52.5,\"lng\":\(13.4 + Double(i) * 0.0003)}") }
        var steps: [String] = []
        var start = 0
        while start < vertices - 1 {
            let end = min(start + 50, vertices - 1)
            steps.append("{\"instruction\":\"Continue \(start)\",\"maneuver\":\"\(start == 0 ? "Depart" : "Straight")\",\"start_index\":\(start),\"end_index\":\(end),\"distance_m\":\(Double(end - start) * 20.0)}")
            start = end
        }
        steps.append("{\"instruction\":\"Arrive\",\"maneuver\":\"Arrive\",\"start_index\":\(vertices - 1),\"end_index\":\(vertices - 1),\"distance_m\":0.0}")
        return "{\"geometry\":[\(geometry.joined(separator: ","))],\"steps\":[\(steps.joined(separator: ","))]}"
    }

    private func benchPull(_ routeJson: String) throws -> Stats {
        let route = try NavSDK.routeFromJson(json: routeJson)
        let sim = try NavSDK.RouteSimulator(route: route, config: NavSDK.SimulatorConfig(speedMps: 5.0))
        let nav = try NavSDK.Navigator(route: route, config: NavSDK.NavigatorConfig())
        for _ in 0..<200 { _ = try nav.updateLocation(raw: sim.nextFix()) }
        var samples = [UInt64](repeating: 0, count: calls)
        for i in 0..<calls {
            let fix = sim.nextFix()
            let t0 = DispatchTime.now().uptimeNanoseconds
            _ = try nav.updateLocation(raw: fix)
            samples[i] = DispatchTime.now().uptimeNanoseconds - t0
        }
        return Stats(samples)
    }

    private final class CountingListener: NavCorePush.NavigationListener {
        var positions = 0
        func onPosition(position: NavCorePush.PositionState) { positions += 1 }
        func onTrip(trip: NavCorePush.TripState) {}
        func onSession(session: NavCorePush.SessionState) {}
        func onError(kind: NavCorePush.ErrorKind, message: String) {}
    }

    private func benchPush(_ routeJson: String) throws -> Stats {
        let route = try NavCorePush.routeFromJson(json: routeJson)
        let sim = try NavCorePush.RouteSimulator(route: route, config: NavCorePush.SimulatorConfig(speedMps: 5.0))
        let controller = try NavigationController(route: route, config: NavCorePush.NavigatorConfig())
        let listener = CountingListener()
        _ = controller.addListener(listener: listener)
        for _ in 0..<200 { controller.updateLocation(raw: sim.nextFix()) }
        var samples = [UInt64](repeating: 0, count: calls)
        for i in 0..<calls {
            let fix = sim.nextFix()
            let t0 = DispatchTime.now().uptimeNanoseconds
            controller.updateLocation(raw: fix)
            samples[i] = DispatchTime.now().uptimeNanoseconds - t0
        }
        XCTAssertEqual(listener.positions, calls + 200)
        return Stats(samples)
    }

    func testBoundaryCost() throws {
        let cases: [(String, String)] = [
            ("route_simple fixture", try fixtureRouteJson()),
            ("straight 2000 vertices", longRouteJson(vertices: 2000)),
        ]
        var lines = [
            "BOUNDARY BENCH (Swift, \(calls) calls each, µs per update_location)",
            "| case                       | approach             |      avg |      p50 |      p99 |      max |",
            "|----------------------------|----------------------|----------|----------|----------|----------|",
        ]
        for (label, route) in cases {
            // Interleave so neither approach benefits from a warmer CPU.
            let pull1 = try benchPull(route), push1 = try benchPush(route)
            let pull2 = try benchPull(route), push2 = try benchPush(route)
            let pull = pull1.p50Us < pull2.p50Us ? pull1 : pull2
            let push = push1.p50Us < push2.p50Us ? push1 : push2
            for (approach, s) in [("pull (UniFFI)", pull), ("push (BoltFFI)", push)] {
                lines.append(String(format: "| %-26@ | %-20@ | %8.1f | %8.1f | %8.1f | %8.1f |",
                                    label as NSString, approach as NSString, s.avgUs, s.p50Us, s.p99Us, s.maxUs))
            }
        }
        let table = lines.joined(separator: "\n")
        print(table)
        if let out = ProcessInfo.processInfo.environment["NAVSDK_BENCH_OUT"] {
            try? table.write(toFile: out, atomically: true, encoding: .utf8)
        }
    }
}
