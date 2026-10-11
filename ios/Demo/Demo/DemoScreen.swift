import NavSDK
import SwiftUI

struct DemoScreen: View {
    @StateObject private var model = DemoModel()

    var body: some View {
        NavigationView {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    Text("core \(coreVersion())")
                        .font(.caption)
                        .foregroundColor(.secondary)

                    Picker("Source", selection: Binding(
                        get: { model.source },
                        set: { model.setSource($0) }
                    )) {
                        ForEach(Source.allCases) { Text($0.rawValue).tag($0) }
                    }
                    .pickerStyle(.segmented)

                    if model.source == .replay {
                        Picker("Fixture", selection: $model.fixtureName) {
                            ForEach(Fixture.names, id: \.self) {
                                Text($0.replacingOccurrences(of: "route_", with: "")).tag($0)
                            }
                        }
                        .pickerStyle(.menu)

                        HStack {
                            Button("Replay 1×") { model.startReplay() }
                                .disabled(model.replaying)
                            Button("Replay 10×") { model.startReplay(speed: 10) }
                                .disabled(model.replaying)
                            Button("Stop") { model.stopReplay() }
                                .disabled(!model.replaying)
                        }
                        .buttonStyle(.bordered)

                        if model.replayTotal > 0 {
                            ProgressView(value: Double(model.replayIndex), total: Double(model.replayTotal))
                            Text("fix \(model.replayIndex) / \(model.replayTotal)")
                                .font(.caption2)
                        }
                    }

                    if model.source == .device {
                        DeviceControls(model: model)
                    }

                    if let session = model.session {
                        TripCard(session: session, stepCount: model.stepCount)
                        if let route = model.activeRoute {
                            CourseMap(session: session, route: route, rawPoint: model.lastRawPoint)
                                .frame(height: 280)
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

            Text("Walk straight, turn right, turn left. You finish about two legs from where you started; walk back and tap again to repeat. Leave the course on purpose to see OFF ROUTE and a reroute.")
                .font(.caption2)
                .foregroundColor(.secondary)
            if model.reroutesInstalled > 0 {
                Text("reroutes installed: \(model.reroutesInstalled)").font(.caption2)
            }
        }
    }
}

/// Schematic map: the route in a local metres frame, the current step
/// highlighted, the raw fix and the snapped position. No map tiles needed.
struct CourseMap: View {
    @ObservedObject var session: NavigationSession
    let route: Route
    let rawPoint: GeoPoint?

    var body: some View {
        Canvas { ctx, size in
            let pts = route.geometry
            guard let origin = pts.first else { return }
            let mPerDegLat = 110_540.0
            let mPerDegLng = 111_320.0 * cos(origin.lat * .pi / 180)
            func enu(_ p: GeoPoint) -> CGPoint {
                CGPoint(x: (p.lng - origin.lng) * mPerDegLng, y: (p.lat - origin.lat) * mPerDegLat)
            }
            var all = pts.map(enu)
            if let raw = rawPoint { all.append(enu(raw)) }
            if let snapped = session.state?.snapped.point { all.append(enu(snapped)) }
            let minX = all.map(\.x).min()!, maxX = all.map(\.x).max()!
            let minY = all.map(\.y).min()!, maxY = all.map(\.y).max()!
            let span = max(maxX - minX, maxY - minY, 20)
            let pad: CGFloat = 24
            let scale = min(size.width, size.height) - 2 * pad
            let k = scale / span
            let cx = (minX + maxX) / 2, cy = (minY + maxY) / 2
            func screen(_ e: CGPoint) -> CGPoint {
                CGPoint(x: size.width / 2 + (e.x - cx) * k, y: size.height / 2 - (e.y - cy) * k)
            }

            // Whole route.
            var path = Path()
            path.addLines(pts.map { screen(enu($0)) })
            ctx.stroke(path, with: .color(.gray.opacity(0.5)), style: StrokeStyle(lineWidth: 3, lineJoin: .round))

            // Current step.
            if let state = session.state {
                let step = route.steps[Int(state.currentStepIndex)]
                let lo = Int(step.startIndex), hi = Int(step.endIndex)
                if hi > lo {
                    var cur = Path()
                    cur.addLines(pts[lo...hi].map { screen(enu($0)) })
                    ctx.stroke(cur, with: .color(.blue), style: StrokeStyle(lineWidth: 5, lineJoin: .round))
                }
            }

            // Manoeuvre points and destination.
            for step in route.steps {
                let p = screen(enu(pts[Int(step.startIndex)]))
                let r: CGFloat = step.maneuver == .arrive ? 7 : 5
                let rect = CGRect(x: p.x - r, y: p.y - r, width: 2 * r, height: 2 * r)
                if step.maneuver == .arrive {
                    ctx.fill(Path(rect), with: .color(.green))
                } else {
                    ctx.fill(Path(ellipseIn: rect), with: .color(.white))
                    ctx.stroke(Path(ellipseIn: rect), with: .color(.gray), lineWidth: 2)
                }
            }

            // Raw fix (hollow red) → snapped (filled blue).
            if let raw = rawPoint {
                let p = screen(enu(raw))
                let rect = CGRect(x: p.x - 6, y: p.y - 6, width: 12, height: 12)
                ctx.stroke(Path(ellipseIn: rect), with: .color(.red), lineWidth: 2)
                if let snapped = session.state?.snapped.point {
                    let q = screen(enu(snapped))
                    var link = Path(); link.move(to: p); link.addLine(to: q)
                    ctx.stroke(link, with: .color(.red.opacity(0.6)), style: StrokeStyle(lineWidth: 1, dash: [3, 3]))
                }
            }
            if let state = session.state {
                let q = screen(enu(state.snapped.point))
                let rect = CGRect(x: q.x - 7, y: q.y - 7, width: 14, height: 14)
                ctx.fill(Path(ellipseIn: rect), with: .color(state.isOffRoute ? .red : .blue))
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

/// Observes the session directly so the card refreshes on every fix.
struct TripCard: View {
    @ObservedObject var session: NavigationSession
    let stepCount: Int

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let state = session.state {
                Text(state.nextInstruction).font(.title2).bold()
                Text("in \(format(state.distanceToNextManeuverM))").font(.title)
                Divider()
                Text("Remaining: \(format(state.distanceRemainingM)) · ETA \(formatEta(state.etaS))")
                Text(String(format: "Speed %.1f km/h", state.speedMps * 3.6))
                Text("Step \(Int(state.currentStepIndex) + 1) of \(stepCount)")
                Text(String(
                    format: "Snapped %.5f, %.5f · %.0f m from route · heading %.0f°",
                    state.snapped.point.lat, state.snapped.point.lng,
                    state.snapped.distanceFromRouteM, state.snapped.bearingDeg
                ))
                .font(.caption)
                .foregroundColor(.secondary)
                HStack {
                    if state.isOffRoute { Badge(text: "OFF ROUTE", color: .red) }
                    if state.progress == .arrived { Badge(text: "ARRIVED", color: .green) }
                    if session.rerouteRequests > 0 {
                        Badge(text: "reroutes requested: \(session.rerouteRequests)", color: .brown)
                    }
                }
            } else {
                Text("Waiting for the first fix…").font(.headline)
            }
            if let error = session.lastError {
                Text("last rejected fix: \(String(describing: error))")
                    .font(.caption2)
                    .foregroundColor(.red)
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
