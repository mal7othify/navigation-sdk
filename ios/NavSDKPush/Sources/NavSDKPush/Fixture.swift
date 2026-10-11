import Foundation

/// A route plus the simulated GPS trace recorded over it (see `fixtures/`),
/// decoded into this module's types. Mirrors `NavSDK.Fixture` for Approach A.
public struct Fixture {
    public let name: String
    public let route: Route
    public let trace: [RawLocation]

    public static let names = ["route_simple", "route_parallel_roads", "route_uturn", "route_detour"]

    public enum LoadError: Error {
        case malformed(String)
    }

    public init(name: String, data: Data) throws {
        guard let root = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let routeObject = root["route"],
              let traceArray = root["trace"] as? [[String: Any]]
        else { throw LoadError.malformed("fixture \(name) is missing route or trace") }

        let routeData = try JSONSerialization.data(withJSONObject: routeObject)
        guard let routeJson = String(data: routeData, encoding: .utf8) else {
            throw LoadError.malformed("route is not UTF-8")
        }
        let route = try routeFromJson(json: routeJson)

        let trace = try traceArray.map { fix -> RawLocation in
            guard let point = fix["point"] as? [String: Any],
                  let lat = point["lat"] as? Double,
                  let lng = point["lng"] as? Double,
                  let ts = fix["timestamp_ms"] as? NSNumber
            else { throw LoadError.malformed("bad fix in \(name)") }
            return RawLocation(
                point: GeoPoint(lat: lat, lng: lng),
                accuracyM: fix["accuracy_m"] as? Double,
                speedMps: fix["speed_mps"] as? Double,
                courseDeg: fix["course_deg"] as? Double,
                timestampMs: ts.uint64Value
            )
        }
        self.name = name
        self.route = route
        self.trace = trace
    }

    /// Load `<name>.json` from a bundle's `Fixtures` directory.
    public init(name: String, bundle: Bundle) throws {
        guard let url = bundle.url(forResource: name, withExtension: "json", subdirectory: "Fixtures")
            ?? bundle.url(forResource: name, withExtension: "json")
        else { throw LoadError.malformed("fixture \(name).json not in bundle") }
        try self.init(name: name, data: try Data(contentsOf: url))
    }
}
