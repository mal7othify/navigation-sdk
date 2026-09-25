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

                    if let session = model.session {
                        TripCard(session: session, stepCount: model.stepCount)
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
                Text("Remaining: \(format(state.distanceRemainingM))")
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
