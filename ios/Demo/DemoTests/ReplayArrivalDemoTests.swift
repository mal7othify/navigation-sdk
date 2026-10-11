import CoreLocation
import NavCorePush
import NavSDK
import XCTest

/// Replays the fixtures bundled with the demo app through both native cores.
final class ReplayArrivalDemoTests: XCTestCase {
    private func fixtureData(_ name: String) throws -> Data {
        let bundle = Bundle(for: ReplayArrivalDemoTests.self)
        let appBundle = Bundle.allBundles.first { $0.bundleIdentifier == "com.navsdk.demo" } ?? bundle
        guard let url = appBundle.url(forResource: name, withExtension: "json", subdirectory: "Fixtures")
            ?? appBundle.url(forResource: name, withExtension: "json")
        else { throw NavSDK.Fixture.LoadError.malformed("\(name).json not in bundle") }
        return try Data(contentsOf: url)
    }

    @MainActor
    func testPullRouteSimpleReachesArrival() async throws {
        let fixture = try NavSDK.Fixture(name: "route_simple", data: fixtureData("route_simple"))
        let session = try NavigationSession(route: fixture.route)
        var last: FullState?
        for raw in fixture.trace {
            last = try await session.updateAndWait(raw)
        }
        XCTAssertEqual(last?.trip.progress, .arrived)
        XCTAssertEqual(session.positionUpdates, fixture.trace.count)
        XCTAssertLessThanOrEqual(session.tripUpdates, fixture.route.steps.count + 1)
    }

    @MainActor
    func testPushRouteSimpleReachesArrivalThroughCallbacks() async throws {
        let fixture = try NavCorePush.Fixture(name: "route_simple", data: fixtureData("route_simple"))
        let session = try NavigationSessionPush(route: fixture.route)
        for raw in fixture.trace {
            let position = await session.updateAndWait(raw)
            XCTAssertNotNil(position)
        }
        XCTAssertEqual(session.position?.progress, .arrived)
        XCTAssertEqual(session.positionCallbacks, fixture.trace.count)
        XCTAssertLessThanOrEqual(session.tripCallbacks, fixture.route.steps.count + 1)
    }

    /// Both approaches over the same fixes must snap to the same place, fix after fix.
    @MainActor
    func testPullAndPushNeverDiverge() async throws {
        let data = try fixtureData("route_simple")
        let a = try NavSDK.Fixture(name: "route_simple", data: data)
        let b = try NavCorePush.Fixture(name: "route_simple", data: data)
        let pull = try NavigationSession(route: a.route)
        let push = try NavigationSessionPush(route: b.route)
        for i in 0..<a.trace.count {
            let pa = try await pull.updateAndWait(a.trace[i]).position
            guard let pb = await push.updateAndWait(b.trace[i]) else { return XCTFail("push rejected fix \(i)") }
            let p = CLLocation(latitude: pa.snapped.point.lat, longitude: pa.snapped.point.lng)
            let q = CLLocation(latitude: pb.snapped.point.lat, longitude: pb.snapped.point.lng)
            XCTAssertLessThan(p.distance(from: q), 0.5, "fix \(i) diverged")
            XCTAssertEqual(pa.distanceRemainingM, pb.distanceRemainingM, accuracy: 1e-6)
            XCTAssertEqual(pa.isOffRoute, pb.isOffRoute)
        }
        XCTAssertEqual(pull.tripUpdates, push.tripCallbacks)
    }
}
