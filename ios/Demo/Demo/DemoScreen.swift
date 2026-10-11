import CoreLocation
import NavCorePush
import NavSDK
import SwiftUI

struct DemoScreen: View {
    @StateObject private var model = DemoModel()

    var body: some View {
        NavigationView {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text("pull core \(NavSDK.coreVersion()) · push core \(NavCorePush.coreVersion())")
                        .font(.caption)
                        .foregroundColor(.secondary)

                    Picker("Approach", selection: $model.approach) {
                        ForEach(Approach.allCases) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)

                    Picker("Source", selection: Binding(
                        get: { model.source },
                        set: { model.setSource($0) }
                    )) {
                        ForEach(Source.allCases) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)

                    if model.source == .device {
                        DeviceControls(model: model)
                    } else {
                        Picker("Fixture", selection: $model.fixtureName) {
                            ForEach(NavSDK.Fixture.names, id: \.self) {
                                Text($0.replacingOccurrences(of: "route_", with: "")).tag($0)
                            }
                        }
                        .pickerStyle(.menu)

                        HStack {
                            Button("Run 1×") { model.start() }.disabled(model.running)
                            Button("Run 10×") { model.start(speed: 10) }.disabled(model.running)
                            Button("Stop") { model.stop() }.disabled(!model.running)
                            if model.source == .simulator {
                                Button("Deviate") { model.deviateNow() }
                                    .disabled(!model.running || model.simulatorDeviating)
                            }
                        }
                        .buttonStyle(.bordered)

                        if model.feedTotal > 0 {
                            ProgressView(value: Double(model.feedIndex), total: Double(model.feedTotal))
                        }
                        Text("fix \(model.feedIndex)\(model.feedTotal > 0 ? " / \(model.feedTotal)" : "")")
                            .font(.caption2)
                    }

                    switch model.approach {
                    case .pull:
                        if let pull = model.pull {
                            PullCard(session: pull, route: model.activeRoute, rawPoint: model.lastRawPoint)
                        }
                    case .push:
                        if let push = model.push {
                            PushCard(session: push, route: model.activeRoute, rawPoint: model.lastRawPoint)
                        }
                    case .both:
                        if let pull = model.pull, let push = model.push {
                            DivergenceStrip(pull: pull, push: push, everDiverged: model.everDiverged)
                            PullCard(session: pull, route: model.activeRoute, rawPoint: model.lastRawPoint)
                            PushCard(session: push, route: model.activeRoute, rawPoint: model.lastRawPoint)
                        }
                    }

                    if let error = model.loadError {
                        Text(error).foregroundColor(.red).font(.caption)
                    }
                }
                .padding()
            }
            .navigationTitle("NavSDK Demo")
        }
    }
}

/// Device mode: GPS status, walking-course setup, auto-reroute toggle.
struct DeviceControls: View {
    @ObservedObject var model: DemoModel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let loc = model.lastDeviceLocation {
                Text(String(
                    format: "GPS %.5f, %.5f · ±%.0f m · %@",
                    loc.coordinate.latitude, loc.coordinate.longitude, loc.horizontalAccuracy,
                    loc.course >= 0 ? String(format: "course %.0f°", loc.course) : "no course yet"
                ))
                .font(.caption)
                .foregroundColor(.secondary)
            } else {
                Text("Waiting for a GPS fix…").font(.caption).foregroundColor(.secondary)
            }

            Picker("Leg", selection: $model.courseLegM) {
                Text("30 m").tag(30.0)
                Text("50 m").tag(50.0)
                Text("100 m").tag(100.0)
            }
            .pickerStyle(.segmented)

            Button("Start walking course here") { model.startWalkCourse() }
                .buttonStyle(.borderedProminent)
                .disabled(model.lastDeviceLocation == nil)

            Toggle("Auto-reroute when the core asks", isOn: $model.autoReroute)
                .font(.subheadline)

            Text(model.courseActive
                ? "Walk straight, turn right, turn left. Leave the course on purpose to see OFF ROUTE and a reroute: the pull wrapper fetches it, the push core fetches it itself."
                : "Walk straight, turn right, turn left. You finish about two legs from where you started; walk back and tap again to repeat.")
                .font(.caption2)
                .foregroundColor(.secondary)
        }
    }
}

/// Instruction-card inputs, identical for both approaches so one card view serves both.
struct CardTrip: Equatable {
    let instruction: String
    let stepIndex: Int
    let stepCount: Int
    let offRoute: Bool
    let arrived: Bool
}

struct CardPosition {
    let toManeuverM: Double
    let remainingM: Double
    let speedMps: Double
    let etaS: Double?
    let lat: Double
    let lng: Double
    let fromRouteM: Double
    let bearingDeg: Double
    let offRoute: Bool
    let arrived: Bool
}

struct CardMetrics {
    let samplesNs: [UInt64]
    let positionUpdates: Int
    let tripUpdates: Int
    let rerouteRequests: Int
    let routesInstalled: Int
    let replanInFlight: Bool
    let lastError: String?

    var calls: Int { positionUpdates }
    var avgUs: Double { samplesNs.isEmpty ? 0 : Double(samplesNs.reduce(0, +)) / Double(samplesNs.count) / 1000 }
    var p99Us: Double {
        guard !samplesNs.isEmpty else { return 0 }
        let sorted = samplesNs.sorted()
        return Double(sorted[Int(Double(sorted.count - 1) * 0.99)]) / 1000
    }
    var maxUs: Double { Double(samplesNs.max() ?? 0) / 1000 }
}

/// Observes the pull session directly so the card refreshes on every fix.
struct PullCard: View {
    @ObservedObject var session: NavigationSession
    let route: DualRoute?
    let rawPoint: CLLocationCoordinate2D?

    var body: some View {
        TripCard(
            title: Approach.pull.rawValue,
            position: session.position.map {
                CardPosition(
                    toManeuverM: $0.distanceToNextManeuverM, remainingM: $0.distanceRemainingM,
                    speedMps: $0.speedMps, etaS: $0.etaS,
                    lat: $0.snapped.point.lat, lng: $0.snapped.point.lng,
                    fromRouteM: $0.snapped.distanceFromRouteM, bearingDeg: $0.snapped.bearingDeg,
                    offRoute: $0.isOffRoute, arrived: $0.progress == .arrived
                )
            },
            trip: session.trip.map {
                CardTrip(
                    instruction: $0.nextInstruction, stepIndex: Int($0.currentStepIndex), stepCount: Int($0.stepCount),
                    offRoute: $0.isOffRoute, arrived: $0.progress == .arrived
                )
            },
            metrics: CardMetrics(
                samplesNs: session.boundarySamplesNs, positionUpdates: session.positionUpdates,
                tripUpdates: session.tripUpdates, rerouteRequests: session.rerouteRequests,
                routesInstalled: session.routesInstalled, replanInFlight: false,
                lastError: session.lastError.map { String(describing: $0) }
            ),
            route: route,
            rawPoint: rawPoint
        )
    }
}

/// Observes the push session directly so the card refreshes on every callback.
struct PushCard: View {
    @ObservedObject var session: NavigationSessionPush
    let route: DualRoute?
    let rawPoint: CLLocationCoordinate2D?

    var body: some View {
        TripCard(
            title: Approach.push.rawValue,
            position: session.position.map {
                CardPosition(
                    toManeuverM: $0.distanceToNextManeuverM, remainingM: $0.distanceRemainingM,
                    speedMps: $0.speedMps, etaS: $0.etaS,
                    lat: $0.snapped.point.lat, lng: $0.snapped.point.lng,
                    fromRouteM: $0.snapped.distanceFromRouteM, bearingDeg: $0.snapped.bearingDeg,
                    offRoute: $0.isOffRoute, arrived: $0.progress == .arrived
                )
            },
            trip: session.trip.map {
                CardTrip(
                    instruction: $0.nextInstruction, stepIndex: Int($0.currentStepIndex), stepCount: Int($0.stepCount),
                    offRoute: $0.isOffRoute, arrived: $0.progress == .arrived
                )
            },
            metrics: CardMetrics(
                samplesNs: session.boundarySamplesNs, positionUpdates: session.positionCallbacks,
                tripUpdates: session.tripCallbacks, rerouteRequests: session.rerouteRequests,
                routesInstalled: Int(session.session.routesInstalled), replanInFlight: session.session.replanInFlight,
                lastError: session.lastError.map { "\($0.kind): \($0.message)" }
            ),
            route: route,
            rawPoint: rawPoint
        )
    }
}

struct DivergenceStrip: View {
    @ObservedObject var pull: NavigationSession
    @ObservedObject var push: NavigationSessionPush
    let everDiverged: Bool

    var body: some View {
        HStack {
            let d = DemoModel.divergence(pull.position, push.position)
            Badge(text: d.map { String(format: "snapped Δ %.2f m", $0) } ?? "snapped Δ —", color: .gray)
            Badge(text: everDiverged ? "DIVERGED" : "identical so far", color: everDiverged ? .red : .green)
        }
    }
}

struct TripCard: View {
    let title: String
    let position: CardPosition?
    let trip: CardTrip?
    let metrics: CardMetrics
    let route: DualRoute?
    let rawPoint: CLLocationCoordinate2D?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.caption).bold()
            if let position, let trip {
                InstructionBlock(trip: trip)
                Text("in \(format(position.toManeuverM))").font(.title)
                Divider()
                Text("Remaining: \(format(position.remainingM)) · ETA \(formatEta(position.etaS))")
                Text(String(format: "Speed %.1f km/h", position.speedMps * 3.6))
                Text(String(
                    format: "Snapped %.5f, %.5f · %.0f m from route · heading %.0f°",
                    position.lat, position.lng, position.fromRouteM, position.bearingDeg
                ))
                .font(.caption)
                .foregroundColor(.secondary)
                HStack {
                    if position.offRoute { Badge(text: "OFF ROUTE", color: .red) }
                    if position.arrived { Badge(text: "ARRIVED", color: .green) }
                    if metrics.replanInFlight { Badge(text: "replanning…", color: .brown) }
                }
            } else {
                Text("Waiting for the first fix…").font(.headline)
            }
            Text(String(
                format: "boundary: %d calls · avg %.1f µs · p99 %.1f µs · max %.1f µs",
                metrics.calls, metrics.avgUs, metrics.p99Us, metrics.maxUs
            ))
            .font(.caption2)
            Text("updates: \(metrics.positionUpdates) position · \(metrics.tripUpdates) trip · reroutes \(metrics.rerouteRequests) · routes installed \(metrics.routesInstalled)")
                .font(.caption2)
            if let error = metrics.lastError {
                Text("last rejected fix: \(error)")
                    .font(.caption2)
                    .foregroundColor(.red)
            }
            if let route {
                CourseMap(
                    route: route,
                    currentStep: trip?.stepIndex,
                    snapped: position.map { CLLocationCoordinate2D(latitude: $0.lat, longitude: $0.lng) },
                    offRoute: position?.offRoute ?? false,
                    rawPoint: rawPoint
                )
                .frame(height: 240)
            }
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 12).fill(Color(.secondarySystemBackground)))
    }

    private func format(_ metres: Double) -> String {
        metres >= 1000 ? String(format: "%.1f km", metres / 1000) : String(format: "%.0f m", metres)
    }

    /// `nil` until the core has seen a few seconds of movement.
    private func formatEta(_ seconds: Double?) -> String {
        guard let s = seconds else { return "—" }
        if s < 60 { return String(format: "%.0f s", s) }
        if s < 3600 { return String(format: "%.0f min", s / 60) }
        return String(format: "%.0f h %02.0f min", (s / 3600).rounded(.down), (s / 60).truncatingRemainder(dividingBy: 60))
    }
}

/// Keyed on the trip only, so its change counter shows how often the slow
/// state really changed, independent of the per-fix numbers below it.
struct InstructionBlock: View {
    let trip: CardTrip
    @State private var changes = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(trip.instruction).font(.title2).bold()
            Text("Step \(trip.stepIndex + 1) of \(trip.stepCount) · instruction changed \(changes)×")
                .font(.caption)
        }
        .onChange(of: trip) { _ in changes += 1 }
    }
}

/// Schematic map: the route in a local metres frame, the current step
/// highlighted, the raw fix and the snapped position. No map tiles needed.
/// Takes plain coordinates so the same map serves both binding layers.
struct CourseMap: View {
    let route: DualRoute
    let currentStep: Int?
    let snapped: CLLocationCoordinate2D?
    let offRoute: Bool
    let rawPoint: CLLocationCoordinate2D?

    var body: some View {
        Canvas { ctx, size in
            let pts = route.points
            guard let origin = pts.first else { return }
            let mPerDegLat = 110_540.0
            let mPerDegLng = 111_320.0 * cos(origin.latitude * .pi / 180)
            func enu(_ p: CLLocationCoordinate2D) -> CGPoint {
                CGPoint(x: (p.longitude - origin.longitude) * mPerDegLng, y: (p.latitude - origin.latitude) * mPerDegLat)
            }
            var all = pts.map(enu)
            if let raw = rawPoint { all.append(enu(raw)) }
            if let snapped { all.append(enu(snapped)) }
            let minX = all.map(\.x).min()!, maxX = all.map(\.x).max()!
            let minY = all.map(\.y).min()!, maxY = all.map(\.y).max()!
            let span = max(maxX - minX, maxY - minY, 20)
            let pad: CGFloat = 24
            let k = (min(size.width, size.height) - 2 * pad) / span
            let cx = (minX + maxX) / 2, cy = (minY + maxY) / 2
            func screen(_ e: CGPoint) -> CGPoint {
                CGPoint(x: size.width / 2 + (e.x - cx) * k, y: size.height / 2 - (e.y - cy) * k)
            }

            // Whole route.
            var path = Path()
            path.addLines(pts.map { screen(enu($0)) })
            ctx.stroke(path, with: .color(.gray.opacity(0.5)), style: StrokeStyle(lineWidth: 3, lineJoin: .round))

            // Current step.
            if let currentStep, currentStep < route.stepRanges.count {
                let range = route.stepRanges[currentStep]
                if range.upperBound > range.lowerBound {
                    var cur = Path()
                    cur.addLines(pts[range].map { screen(enu($0)) })
                    ctx.stroke(cur, with: .color(.blue), style: StrokeStyle(lineWidth: 5, lineJoin: .round))
                }
            }

            // Manoeuvre points and destination.
            for (i, range) in route.stepRanges.enumerated() {
                let p = screen(enu(pts[range.lowerBound]))
                let isArrive = i == route.stepRanges.count - 1
                let r: CGFloat = isArrive ? 7 : 5
                let rect = CGRect(x: p.x - r, y: p.y - r, width: 2 * r, height: 2 * r)
                if isArrive {
                    ctx.fill(Path(rect), with: .color(.green))
                } else {
                    ctx.fill(Path(ellipseIn: rect), with: .color(.white))
                    ctx.stroke(Path(ellipseIn: rect), with: .color(.gray), lineWidth: 2)
                }
            }

            // Raw fix (hollow red) → snapped (filled blue; red when off route).
            if let raw = rawPoint {
                let p = screen(enu(raw))
                let rect = CGRect(x: p.x - 6, y: p.y - 6, width: 12, height: 12)
                ctx.stroke(Path(ellipseIn: rect), with: .color(.red), lineWidth: 2)
                if let snapped {
                    let q = screen(enu(snapped))
                    var link = Path(); link.move(to: p); link.addLine(to: q)
                    ctx.stroke(link, with: .color(.red.opacity(0.6)), style: StrokeStyle(lineWidth: 1, dash: [3, 3]))
                }
            }
            if let snapped {
                let q = screen(enu(snapped))
                let rect = CGRect(x: q.x - 7, y: q.y - 7, width: 14, height: 14)
                ctx.fill(Path(ellipseIn: rect), with: .color(offRoute ? .red : .blue))
            }

            // North arrow and 10 m scale bar.
            ctx.draw(Text("N").font(.caption2.bold()), at: CGPoint(x: size.width - 14, y: 12))
            var north = Path()
            north.move(to: CGPoint(x: size.width - 14, y: 32)); north.addLine(to: CGPoint(x: size.width - 14, y: 20))
            ctx.stroke(north, with: .color(.primary), lineWidth: 1.5)
            var bar = Path()
            bar.move(to: CGPoint(x: 12, y: size.height - 12)); bar.addLine(to: CGPoint(x: 12 + 10 * k, y: size.height - 12))
            ctx.stroke(bar, with: .color(.primary), lineWidth: 2)
            ctx.draw(Text("10 m").font(.caption2), at: CGPoint(x: 12 + 5 * k, y: size.height - 22))
        }
        .background(Color(.secondarySystemBackground))
        .cornerRadius(12)
    }
}

struct Badge: View {
    let text: String
    let color: Color
    var body: some View {
        Text(text)
            .font(.caption).bold()
            .padding(.horizontal, 8).padding(.vertical, 4)
            .background(Capsule().fill(color))
            .foregroundColor(.white)
    }
}
