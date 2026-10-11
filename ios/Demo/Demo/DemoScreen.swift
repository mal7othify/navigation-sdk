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
                        ForEach(Source.allCases) { source in
                            // Device location feeds a single session; Both needs lockstep replay.
                            if source != .device || model.approach != .both {
                                Text(source.rawValue).tag(source)
                            }
                        }
                    }
                    .pickerStyle(.segmented)

                    if model.source != .device {
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
                        if let pull = model.pull { PullCard(session: pull) }
                    case .push:
                        if let push = model.push { PushCard(session: push) }
                    case .both:
                        if let pull = model.pull, let push = model.push {
                            DivergenceStrip(pull: pull, push: push, everDiverged: model.everDiverged)
                            PullCard(session: pull)
                            PushCard(session: push)
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

    var body: some View {
        TripCard(
            title: Approach.pull.rawValue,
            position: session.position.map {
                CardPosition(
                    toManeuverM: $0.distanceToNextManeuverM, remainingM: $0.distanceRemainingM,
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
            )
        )
    }
}

/// Observes the push session directly so the card refreshes on every callback.
struct PushCard: View {
    @ObservedObject var session: NavigationSessionPush

    var body: some View {
        TripCard(
            title: Approach.push.rawValue,
            position: session.position.map {
                CardPosition(
                    toManeuverM: $0.distanceToNextManeuverM, remainingM: $0.distanceRemainingM,
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
            )
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

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.caption).bold()
            if let position, let trip {
                InstructionBlock(trip: trip)
                Text("in \(format(position.toManeuverM))").font(.title)
                Divider()
                Text("Remaining: \(format(position.remainingM))")
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
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 12).fill(Color(.secondarySystemBackground)))
    }

    private func format(_ metres: Double) -> String {
        metres >= 1000 ? String(format: "%.1f km", metres / 1000) : String(format: "%.0f m", metres)
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
